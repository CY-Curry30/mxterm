use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

struct ExecServer {
    channels: Vec<Channel<russh::server::Msg>>,
    foreground: Option<russh::ChannelId>,
    foreground_closed: Arc<AtomicBool>,
    finish_background: Arc<Notify>,
}

impl russh::server::Handler for ExecServer {
    type Error = russh::Error;

    async fn auth_none(&mut self, _: &str) -> Result<russh::server::Auth, Self::Error> {
        Ok(russh::server::Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<russh::server::Msg>,
        _: &mut russh::server::Session,
    ) -> Result<bool, Self::Error> {
        self.channels.push(channel);
        Ok(true)
    }

    async fn exec_request(
        &mut self,
        channel: russh::ChannelId,
        command: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        match command {
            b"foreground" => {
                self.foreground = Some(channel);
                session.data(channel, b"PARTIAL_OUTPUT".to_vec())?;
                // Deliberately leave the channel open without exit status.
            }
            b"background" => {
                let handle = session.handle();
                let finish = self.finish_background.clone();
                tokio::spawn(async move {
                    finish.notified().await;
                    handle
                        .data(channel, b"BACKGROUND_DONE".to_vec())
                        .await
                        .unwrap();
                    handle.exit_status_request(channel, 0).await.unwrap();
                    handle.eof(channel).await.unwrap();
                    handle.close(channel).await.unwrap();
                });
            }
            b"next" => {
                session.data(channel, b"NEXT".to_vec())?;
                session.exit_status_request(channel, 0)?;
                session.eof(channel)?;
                session.close(channel)?;
            }
            _ => panic!("unexpected test command"),
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: russh::ChannelId,
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if self.foreground == Some(channel) {
            self.foreground_closed.store(true, Ordering::SeqCst);
            self.finish_background.notify_one();
        }
        Ok(())
    }
}

#[test]
fn preemption_closes_only_the_foreground_ssh_channel_and_reuses_the_connection() {
    check_foreground_channel_cleanup(false);
}

#[test]
fn dropped_read_future_closes_only_its_ssh_channel_and_reuses_the_connection() {
    check_foreground_channel_cleanup(true);
}

fn check_foreground_channel_cleanup(drop_read: bool) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = tempfile::tempdir().unwrap();
        let keypair = russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[43; 32]);
        let key = russh::keys::ssh_key::PrivateKey::new(keypair.into(), "exec test only").unwrap();
        let secrets: Arc<dyn SecretStore> =
            Arc::new(crate::storage_vault::InMemorySecretStore::default());
        StorageRepository::open_root(root.path(), secrets.clone())
            .unwrap()
            .known_host_trust(
                host_key_info("exec-test.invalid", 22, key.public_key()),
                "test",
            )
            .unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        let server_closed = closed.clone();
        let (client_stream, server_stream) = tokio::io::duplex(65536);
        let server = tokio::spawn(async move {
            russh::server::run_stream(
                Arc::new(russh::server::Config {
                    keys: vec![key],
                    ..Default::default()
                }),
                server_stream,
                ExecServer {
                    channels: Vec::new(),
                    foreground: None,
                    foreground_closed: server_closed,
                    finish_background: Arc::new(Notify::new()),
                },
            )
            .await
            .unwrap()
        });
        let mut client = client::connect_stream(
            Arc::new(client::Config::default()),
            client_stream,
            KnownHostClient {
                host: "exec-test.invalid".into(),
                port: 22,
                app_data_dir: root.path().into(),
                secret_store: secrets,
                remote_forward: RemoteForwardState::default(),
                x11: None,
                x11_error: None,
            },
        )
        .await
        .unwrap();
        assert!(client.authenticate_none("test").await.unwrap().success());
        let exec = ReusableExecSession {
            client,
            jump_client: None,
        };
        let mut background = exec.client.channel_open_session().await.unwrap();
        background.exec(true, "background").await.unwrap();
        let (signal, receiver) = watch::channel(false);
        let operation = async {
            if drop_read {
                let partial = Arc::new(std::sync::Mutex::new(Vec::new()));
                let captured = partial.clone();
                let read = exec.exec_with_stdout_chunks(
                    "foreground",
                    Arc::new(move |bytes| {
                        captured.lock().unwrap().extend_from_slice(bytes);
                        signal.send_replace(true);
                    }),
                );
                tokio::pin!(read);
                tokio::select! {
                    _ = wait_for_cancellation(receiver) => {},
                    _ = &mut read => panic!("read completed before cancellation"),
                }
                // Dropping a read future must release its command channel too.
                let stdout = partial.lock().unwrap().clone();
                Ok((
                    ExecOutput {
                        stdout,
                        stderr: Vec::new(),
                        exit_status: None,
                    },
                    true,
                ))
            } else {
                exec.exec_with_stdout_chunks_cancellable(
                    "foreground",
                    Arc::new(move |bytes| {
                        assert_eq!(bytes, b"PARTIAL_OUTPUT");
                        signal.send_replace(true);
                    }),
                    receiver,
                )
                .await
            }
        };
        let (output, cancelled) = tokio::time::timeout(Duration::from_secs(3), operation)
            .await
            .unwrap()
            .unwrap();
        assert!(cancelled);
        assert_eq!(output.stdout, b"PARTIAL_OUTPUT");
        assert_eq!(output.exit_status, None);
        let next = tokio::time::timeout(Duration::from_secs(3), exec.exec("next"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.stdout, b"NEXT");
        assert_eq!(next.exit_status, Some(0));
        let (mut background_output, mut background_status) = (Vec::new(), None);
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(message) = background.wait().await {
                match message {
                    ChannelMsg::Data { data } => background_output.extend_from_slice(&data),
                    ChannelMsg::ExitStatus { exit_status } => background_status = Some(exit_status),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(
            closed.load(Ordering::SeqCst),
            "server never received foreground channel close"
        );
        assert_eq!(background_output, b"BACKGROUND_DONE");
        assert_eq!(background_status, Some(0));
        exec.client
            .disconnect(Disconnect::ByApplication, "", "en")
            .await
            .unwrap();
        let running = server.await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), running)
            .await
            .unwrap()
            .unwrap();
    });
}
