use super::*;

pub(super) fn start_endpoint_transport(
    stream: LocalStream,
    lifetime: impl Send + 'static,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    endpoint_id: endpoint::ClientEndpointId,
    generation: u64,
    max_frame_size: usize,
) -> Result<endpoint::NativeEndpointTransport, ClientError> {
    let reader = stream.try_clone().map_err(ClientError::ConnectionFailed)?;
    let transport = endpoint::NativeEndpointTransport::with_lifetime(stream, lifetime)
        .map_err(ClientError::ConnectionFailed)?;
    let stopped = transport.stop_handle();
    let event_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("endpoint-reader".into())
        .spawn(move || {
            server_reader_thread(
                reader,
                event_tx,
                &stopped,
                max_frame_size,
                endpoint_id,
                generation,
            );
        })
        .map_err(ClientError::ConnectionFailed)?;
    Ok(transport)
}

/// Reads complete frames while retaining partial-read progress across nonblocking polls.
pub(super) fn server_reader_thread(
    mut stream: LocalStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    max_frame_size: usize,
    endpoint_id: endpoint::ClientEndpointId,
    generation: u64,
) {
    if stream.set_nonblocking(true).is_err() {
        let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected {
            endpoint_id,
            generation,
        });
        return;
    }

    let mut stream = EndpointReader {
        stream: &mut stream,
        stopped: should_quit,
    };
    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }

        match protocol::read_message(&mut stream, max_frame_size) {
            Ok(msg) => {
                if event_tx
                    .blocking_send(ClientLoopEvent::ServerMessage {
                        endpoint_id: endpoint_id.clone(),
                        generation,
                        message: Box::new(msg),
                    })
                    .is_err()
                {
                    break;
                }
            }
            Err(protocol::FramingError::UnexpectedEof) => {
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected {
                    endpoint_id: endpoint_id.clone(),
                    generation,
                });
                break;
            }
            Err(protocol::FramingError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(err) => {
                warn!(err = %err, "server read error");
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected {
                    endpoint_id: endpoint_id.clone(),
                    generation,
                });
                break;
            }
        }
    }
}

struct EndpointReader<'a> {
    stream: &'a mut LocalStream,
    stopped: &'a AtomicBool,
}

impl io::Read for EndpointReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return Ok(0);
            }
            match crate::ipc::poll_local_stream_read_count(self.stream, buffer)? {
                crate::ipc::LocalStreamReadCount::Data(count) => return Ok(count),
                crate::ipc::LocalStreamReadCount::Closed => return Ok(0),
                crate::ipc::LocalStreamReadCount::Pending => {
                    crate::platform::wait_client_stream_readable(self.stream)?;
                }
            }
        }
    }
}

pub(in crate::client) fn write_to_local_server(
    stream: &mut LocalStream,
    msg: &ClientMessage,
) -> io::Result<()> {
    protocol::write_message(stream, msg).map_err(|error| io::Error::other(error.to_string()))
}

pub(super) trait ClientMessageSink {
    fn send_client_message(&mut self, message: &ClientMessage) -> io::Result<()>;
}

impl ClientMessageSink for LocalStream {
    fn send_client_message(&mut self, message: &ClientMessage) -> io::Result<()> {
        write_to_local_server(self, message)
    }
}

impl ClientMessageSink for endpoint::EndpointRegistry {
    fn send_client_message(&mut self, message: &ClientMessage) -> io::Result<()> {
        // The lifecycle loop consumes failures for every endpoint, including Local. A send
        // failure must not bypass that transition or tear down unrelated connections.
        self.send(message);
        Ok(())
    }
}

pub(super) fn write_to_server(
    stream: &mut impl ClientMessageSink,
    msg: &ClientMessage,
) -> io::Result<()> {
    stream.send_client_message(msg)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::client::endpoint::EndpointTransport as _;
    use interprocess::local_socket::traits::Listener as _;
    use std::io::{Read as _, Write as _};
    use std::time::Instant;

    #[test]
    fn atomic_scene_publication_is_one_client_event_not_three_messages() {
        use crate::protocol::scene::SceneProjectionV1;
        let mut render = crate::server::render_stream::ClientRenderState::new(
            crate::protocol::RenderEncoding::SemanticFrame,
        );
        let surface = crate::protocol::PaneSurfaceFrame {
            boot_id: "prototype-boot".into(),
            projection_revision: 2,
            surface_revision: 0,
            frame: crate::protocol::FrameData {
                width: 0,
                height: 0,
                cells: Vec::new(),
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            panes: Vec::new(),
            collections: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: Default::default(),
        };
        let prepared = render.prepare_pane_surface(surface, Vec::new()).unwrap();
        let prepared = prepared.with_scene(
            7,
            vec![
                SceneProjectionV1 {
                    kind: "snapshot-prototype".into(),
                    data: "S2".into(),
                },
                SceneProjectionV1 {
                    kind: "collapse-prototype".into(),
                    data: "C2".into(),
                },
            ],
            Vec::new(),
        );
        let path = std::env::temp_dir().join(format!("scene-{}.sock", std::process::id()));
        let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let mut server = listener.accept().unwrap();
        std::fs::remove_file(path).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let stopped = Arc::new(AtomicBool::new(false));
        let reader_stopped = stopped.clone();
        let reader = std::thread::spawn(move || {
            server_reader_thread(
                client,
                tx,
                &reader_stopped,
                protocol::MAX_FRAME_SIZE,
                endpoint::ClientEndpointId::Local,
                1,
            )
        });
        protocol::write_message(&mut server, prepared.message()).unwrap();
        render.commit_sent_frame(prepared);
        let ClientLoopEvent::ServerMessage { message, .. } = rx.blocking_recv().unwrap() else {
            panic!("one scene event");
        };
        let ServerMessage::ClientSceneV1(scene) = *message else {
            panic!("scene codec");
        };
        assert_eq!(scene.route_epoch, 7);
        assert_eq!(scene.surface.frame.projection_revision, 2);
        assert_eq!(scene.surface.frame.surface_revision, 1);
        assert_eq!(scene.projections.len(), 2);
        assert!(
            rx.try_recv().is_err(),
            "no separately schedulable S2/F2/C2 events"
        );
        stopped.store(true, Ordering::Release);
        drop(server);
        reader.join().unwrap();
    }

    #[test]
    fn upload_cancellation_preserves_pending_endpoint_download() {
        let path = std::env::temp_dir().join(format!(
            "herdr-cancel-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let mut bridge = listener.accept().unwrap();
        std::fs::remove_file(path).unwrap();
        drop(listener);
        let mut reader_stream = client.try_clone().unwrap();
        let mut writer = endpoint::NativeEndpointTransport::with_lifetime(client, ()).unwrap();
        let stopped = writer.stop_handle();
        struct ForwardedInput(std::sync::mpsc::Sender<Vec<u8>>);
        impl io::Write for ForwardedInput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.send(bytes.to_vec()).unwrap();
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (forwarded_tx, forwarded_rx) = std::sync::mpsc::channel();
        let cancel = crate::remote::bridge_upload_cancellation_for_test(
            bridge.try_clone().unwrap(),
            ForwardedInput(forwarded_tx),
        );
        let message = ClientMessage::ClientShellFocus { focused: false };
        let mut expected = Vec::new();
        protocol::write_message(&mut expected, &message).unwrap();
        writer.send(&message).unwrap();
        let mut forwarded = Vec::new();
        while forwarded.len() < expected.len() {
            forwarded.extend(forwarded_rx.recv_timeout(Duration::from_secs(3)).unwrap());
        }
        assert_eq!(forwarded, expected);
        cancel();

        // A client write after upload cancellation must not stop the download reader.
        writer
            .send(&ClientMessage::ClientShellFocus { focused: true })
            .unwrap();
        let flushed = writer.flush(Instant::now() + Duration::from_secs(3));
        if flushed.is_ok() {
            let received: ClientMessage =
                protocol::read_message(&mut bridge, protocol::MAX_FRAME_SIZE).unwrap();
            assert_eq!(received, ClientMessage::ClientShellFocus { focused: true });
        }
        const FINAL: &[u8] = b"pending-download: FINAL OUTPUT\n";
        bridge.write_all(FINAL).unwrap();
        drop(bridge);
        let mut output = Vec::new();
        EndpointReader {
            stream: &mut reader_stream,
            stopped: &stopped,
        }
        .read_to_end(&mut output)
        .unwrap();
        assert_eq!(output, FINAL);
        assert!(flushed.is_ok(), "client write failed: {flushed:?}");
        assert!(!stopped.load(Ordering::Acquire));
        assert!(writer.take_error().is_none());
    }
}
