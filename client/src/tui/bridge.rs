//! The chat stream as a message stream.
//!
//! v1's `run_stream_bridge` was an async task threaded with a
//! `watch`-channel cancellation protocol (checked before the context
//! gather and on every frame). Here the bridge is a plain
//! `Stream<Item = Msg>` driven by `ctx.spawn`: dropping the app's `Task`
//! drops this generator at whatever await point it's suspended on, which
//! aborts the HTTP stream.

use futures::StreamExt;

use crate::context::{AppContext, ClientContext};
use crate::fsm::events::Event;
use crate::skills::SkillSummary;
use crate::stream::{ChatRequest, StreamContent, StreamControl, StreamFrame, create_chat_stream};
use crate::tui::app::Msg;

pub fn stream_bridge(
    request: ChatRequest,
    app_ctx: AppContext,
    client_ctx: ClientContext,
    skill_summaries: Vec<SkillSummary>,
    skill_overflow: Option<String>,
) -> impl futures::Stream<Item = Msg> + Send {
    async_stream::stream! {
        let stream = create_chat_stream(
            app_ctx.endpoint.clone(),
            app_ctx.token.clone(),
            request,
            client_ctx,
            app_ctx.send_cwd,
            app_ctx.last_command.clone(),
            skill_summaries,
            skill_overflow,
        );
        futures::pin_mut!(stream);

        yield Msg::Fsm(Event::StreamStarted);

        while let Some(frame) = stream.next().await {
            let event = match frame {
                Ok(StreamFrame::Content(content)) => match content {
                    StreamContent::TextChunk(text) => Some(Event::StreamChunk(text)),
                    StreamContent::ToolCall { id, name, input } => {
                        if name == "suggest_command" {
                            Some(Event::SuggestCommand { id, input })
                        } else {
                            Some(Event::StreamToolCall { id, name, input })
                        }
                    }
                    StreamContent::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        remote,
                        content_length,
                    } => Some(Event::StreamServerToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        remote,
                        content_length,
                    }),
                },
                Ok(StreamFrame::Control(control)) => match control {
                    StreamControl::StatusChanged(status) => {
                        Some(Event::StreamStatusChanged(status))
                    }
                    StreamControl::Done { session_id } => Some(Event::StreamDone { session_id }),
                    StreamControl::Error(msg) => Some(Event::StreamError(msg)),
                },
                Ok(StreamFrame::SessionIdentity(session_id)) => {
                    Some(Event::SessionIdReceived(session_id))
                }
                Err(e) => Some(Event::StreamError(e.to_string())),
            };

            if let Some(event) = event {
                // StreamDone and StreamError are terminal — the server won't
                // send more. SuggestCommand is NOT terminal: StreamDone
                // follows it with the session_id we need to capture.
                let is_terminal =
                    matches!(event, Event::StreamDone { .. } | Event::StreamError(_));
                yield Msg::Fsm(event);
                if is_terminal {
                    return;
                }
            }
        }

        // Cut off before `done` or `error`: without an error the turn waits forever, input off.
        yield Msg::Fsm(Event::StreamError(
            "The server closed the response before finishing it".to_owned(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;
    use crate::stream::ChatRequest;

    /// A local server that answers one request with the SSE `body`, then hangs up.
    async fn serve_once(body: &'static str) -> reqwest::Url {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a local port");
        let addr = listener.local_addr().expect("the listener's address");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("a connection");
            let mut request = [0; 16 * 1024];
            let _ = socket.read(&mut request).await;
            let head =
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
            socket.write_all(format!("{head}{body}").as_bytes()).await.expect("the answer is sent");
        });
        format!("http://{addr}").parse().expect("a URL")
    }

    fn app_ctx(endpoint: reqwest::Url) -> AppContext {
        AppContext {
            endpoint,
            token: None,
            send_cwd: false,
            last_command: None,
            history_db: None,
            vcs_root: None,
            capabilities: crate::settings::AiCapabilities::default(),
            yolo: false,
        }
    }

    #[rstest]
    #[case::cut_off("event: text\ndata: {\"content\":\"hi\"}\n\n", true)]
    #[case::done("event: done\ndata: {\"session_id\":\"s1\"}\n\n", false)]
    #[tokio::test]
    async fn a_stream_cut_off_before_done_ends_in_an_error(
        #[case] body: &'static str,
        #[case] expect_error: bool,
    ) {
        let request = ChatRequest {
            messages: Vec::new(),
            session_id: None,
            capabilities: Vec::new(),
            invocation_id: "test".to_owned(),
            model: None,
        };
        let client_ctx = ClientContext {
            os: "linux".to_owned(),
            shell: None,
            distro: None,
        };
        let ctx = app_ctx(serve_once(body).await);
        let msgs: Vec<Msg> =
            stream_bridge(request, ctx, client_ctx, Vec::new(), None).collect().await;

        let last = msgs.last().expect("the bridge yields messages");
        assert_eq!(matches!(last, Msg::Fsm(Event::StreamError(_))), expect_error, "{last:?}");
    }
}
