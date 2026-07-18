use bytes::Bytes;
use russh::{client::Msg, Channel, ChannelMsg};

use super::{TransportEvent, TransportRequest};

/// Drives one russh session channel with uniform write, EOF, exit, and fault
/// semantics for both direct and proxy connections.
pub(crate) async fn run(
    mut channel: Channel<Msg>,
    requests: flume::Receiver<TransportRequest>,
    events: flume::Sender<TransportEvent>,
    description: &'static str,
) {
    loop {
        tokio::select! {
            Ok(request) = requests.recv_async() => {
                match request {
                    TransportRequest::Write { data, written } => {
                        match channel.data(data.as_ref()).await {
                            Ok(()) => {
                                let _ = written.send(Ok(()));
                            }
                            Err(error) => {
                                let message = format!("{} write failed: {}", description, error);
                                let _ = written.send(Err(anyhow::anyhow!(message.clone())));
                                let _ = events.send_async(TransportEvent::Fault(message)).await;
                                break;
                            }
                        }
                    }
                }
            }
            Some(message) = channel.wait() => {
                match message {
                    ChannelMsg::Data { ref data } => {
                        if events
                            .send_async(TransportEvent::Stdout(Bytes::from(data.to_vec())))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    ChannelMsg::ExtendedData { ref data, .. } => {
                        if events
                            .send_async(TransportEvent::Stderr(Bytes::from(data.to_vec())))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    ChannelMsg::Eof => {
                        let _ = events.send_async(TransportEvent::Exited(None)).await;
                        break;
                    }
                    ChannelMsg::ExitStatus { exit_status } => {
                        let _ = events
                            .send_async(TransportEvent::Exited(Some(exit_status)))
                            .await;
                        break;
                    }
                    _ => {}
                }
            }
            else => break,
        }
    }
}
