use super::*;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = socket.read(&mut buffer).await.unwrap();
        assert!(count > 0, "request ended before its body");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}

fn provider(endpoint: String, format: AiApiFormat) -> StoredAiProviderConfig {
    serde_json::from_value(json!({
        "id": "preemption-test", "name": "test", "provider": "openai",
        "api_format": format, "endpoint": endpoint, "model": "test-model",
        "created_at": "", "updated_at": ""
    }))
    .unwrap()
}

fn stream_events(format: AiApiFormat) -> (Vec<Value>, Vec<Value>) {
    match format {
        AiApiFormat::OpenaiCompatible => (
            vec![
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"unfinished","function":{"name":"run_command","arguments":"{\"command\":"}}]}}]}),
                json!({"choices":[{"delta":{"content":"partial reply"}}]}),
            ],
            vec![json!({"choices":[{"delta":{"content":"new reply"}}]})],
        ),
        AiApiFormat::Responses => (
            vec![
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"unfinished","name":"run_command","arguments":"{\"command\":"}}),
                json!({"type":"response.output_text.delta","delta":"partial reply"}),
            ],
            vec![
                json!({"type":"response.output_text.delta","delta":"new reply"}),
                json!({"type":"response.completed"}),
            ],
        ),
        AiApiFormat::Anthropic => (
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"unfinished","name":"run_command","input":{}}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}),
                json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"partial reply"}}),
            ],
            vec![
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"new reply"}}),
                json!({"type":"message_stop"}),
            ],
        ),
    }
}

async fn write_events(socket: &mut TcpStream, events: Vec<Value>) {
    socket
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    for event in events {
        socket
            .write_all(format!("data: {event}\n\n").as_bytes())
            .await
            .unwrap();
    }
}

#[test]
fn preemption_drops_stream_and_unfinished_tools_then_requests_new_message_in_all_formats() {
    runtime().block_on(async {
        for format in [
            AiApiFormat::OpenaiCompatible,
            AiApiFormat::Responses,
            AiApiFormat::Anthropic,
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let config = provider(
                format!("http://{}/v1", listener.local_addr().unwrap()),
                format,
            );
            let (partial, complete) = stream_events(format);
            let server = tokio::spawn(async move {
                let (mut first, _) = listener.accept().await.unwrap();
                let old_request = read_request(&mut first).await;
                write_events(&mut first, partial).await;
                let mut byte = [0u8; 1];
                // The first response has no finish event; only cancellation can close it.
                assert_eq!(
                    timeout(Duration::from_secs(3), first.read(&mut byte))
                        .await
                        .unwrap()
                        .unwrap(),
                    0
                );
                let (mut second, _) = listener.accept().await.unwrap();
                let new_request = read_request(&mut second).await;
                write_events(&mut second, complete).await;
                (old_request, new_request)
            });
            let client = Client::builder().no_proxy().build().unwrap();
            let (signal, receiver) = watch::channel(false);
            let mut conversation =
                AgentConversation::new(format, Vec::new(), MAX_CONTEXT_WINDOW_TOKENS);
            conversation.push_user_message(AiModelMessage {
                role: "user".into(),
                content: "old task".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
            let (turn, interrupted) = timeout(
                Duration::from_secs(3),
                run_turn_with_preemption(
                    &client,
                    &config,
                    "test-key",
                    None,
                    "test",
                    &conversation.messages,
                    Arc::new(AtomicBool::new(false)),
                    receiver.clone(),
                    |_| {
                        signal.send_replace(true);
                    },
                    |_| {},
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(interrupted);
            assert_eq!(turn.text, "partial reply");
            assert!(turn.tool_calls.is_empty());
            conversation.push_assistant_turn(&turn);
            conversation.push_user_message(AiModelMessage {
                role: "user".into(),
                content: "new task".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
            signal.send_replace(false);
            let (next, interrupted) = timeout(
                Duration::from_secs(3),
                run_turn_with_preemption(
                    &client,
                    &config,
                    "test-key",
                    None,
                    "test",
                    &conversation.messages,
                    Arc::new(AtomicBool::new(false)),
                    receiver,
                    |_| {},
                    |_| {},
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(!interrupted);
            assert_eq!(next.text, "new reply");
            let (old_request, new_request) = timeout(Duration::from_secs(3), server)
                .await
                .unwrap()
                .unwrap();
            assert!(!old_request.to_string().contains("new task"));
            let history = if format == AiApiFormat::Responses {
                &new_request["input"]
            } else {
                &new_request["messages"]
            };
            let serialized = history.to_string();
            assert!(serialized.contains("partial reply"));
            assert!(serialized.contains("new task"));
            assert!(!serialized.contains("unfinished"));
        }
    });
}

#[test]
fn preemption_cancels_provider_waiting_for_response_headers() {
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = provider(
            format!("http://{}/v1", listener.local_addr().unwrap()),
            AiApiFormat::OpenaiCompatible,
        );
        let (signal, receiver) = watch::channel(false);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            signal.send_replace(true);
            // Deliberately never return response headers.
            std::future::pending::<()>().await;
        });
        let (turn, interrupted) = timeout(
            Duration::from_secs(3),
            run_turn_with_preemption(
                &Client::builder().no_proxy().build().unwrap(),
                &config,
                "test-key",
                None,
                "test",
                &[],
                Arc::new(AtomicBool::new(false)),
                receiver,
                |_| {},
                |_| {},
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(interrupted);
        assert!(turn.text.is_empty() && turn.tool_calls.is_empty());
        server.abort();
    });
}

#[test]
fn preemption_cancels_approval_waits_and_wins_over_an_already_delivered_answer() {
    runtime().block_on(async {
        let (signal, receiver) = watch::channel(false);
        let (sender, input) = oneshot::channel::<bool>();
        signal.send_replace(true);
        assert_eq!(
            timeout(
                Duration::from_secs(1),
                wait_for_agent_input(input, receiver.clone())
            )
            .await
            .unwrap(),
            None
        );
        assert!(sender.send(true).is_err());
        let (sender, input) = oneshot::channel::<bool>();
        sender.send(true).unwrap();
        assert_eq!(wait_for_agent_input(input, receiver.clone()).await, None);
        signal.send_replace(false);
        let (sender, input) = oneshot::channel::<bool>();
        sender.send(true).unwrap();
        assert_eq!(wait_for_agent_input(input, receiver).await, Some(true));
    });
}

#[test]
fn preemption_keeps_a_result_for_every_tool_before_new_user_message() {
    let turn = AgentTurn {
        tool_calls: vec![
            AgentToolCall {
                id: "completed".into(),
                name: "apply_patch".into(),
                arguments: "{}".into(),
            },
            AgentToolCall {
                id: "cancelled".into(),
                name: "run_command".into(),
                arguments: "{}".into(),
            },
            AgentToolCall {
                id: "skipped".into(),
                name: "run_command".into(),
                arguments: "{}".into(),
            },
        ],
        ..AgentTurn::default()
    };
    for format in [
        AiApiFormat::OpenaiCompatible,
        AiApiFormat::Responses,
        AiApiFormat::Anthropic,
    ] {
        let mut conversation =
            AgentConversation::new(format, Vec::new(), MAX_CONTEXT_WINDOW_TOKENS);
        conversation.push_assistant_turn(&turn);
        conversation.push_tool_results(
            &turn
                .tool_calls
                .iter()
                .enumerate()
                .map(|(i, call)| {
                    (
                        call.clone(),
                        ToolOutcome {
                            content: if i == 0 { "written" } else { "cancelled" }.into(),
                            is_error: i != 0,
                        },
                    )
                })
                .collect::<Vec<_>>(),
        );
        conversation.push_user_message(AiModelMessage {
            role: "user".into(),
            content: "new task".into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
        let results: Vec<Value> = match format {
            AiApiFormat::Anthropic => conversation.messages[1]["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|value| value["type"] == "tool_result")
                .cloned()
                .collect(),
            _ => conversation
                .messages
                .iter()
                .filter(|value| value["role"] == "tool" || value["type"] == "function_call_output")
                .cloned()
                .collect(),
        };
        assert_eq!(results.len(), 3);
        for (result, id) in results.iter().zip(["completed", "cancelled", "skipped"]) {
            let result_id = match format {
                AiApiFormat::OpenaiCompatible => &result["tool_call_id"],
                AiApiFormat::Responses => &result["call_id"],
                AiApiFormat::Anthropic => &result["tool_use_id"],
            };
            assert_eq!(result_id, id);
        }
        assert!(conversation
            .messages
            .last()
            .unwrap()
            .to_string()
            .contains("new task"));
    }
}

#[test]
fn preemption_approval_outcome_preserves_the_real_cancellation_reason() {
    let mut record = AiToolCallRecord::new("test", "apply_patch", 0);
    record.status = TOOL_STATUS_CANCELLED.into();
    record.error = Some("new message interrupted approval".into());
    let outcome = approval_denied_outcome(&record, "user rejected");
    assert!(outcome.is_error);
    assert_eq!(outcome.content, "new message interrupted approval");
    record.status = TOOL_STATUS_REJECTED.into();
    let outcome = approval_denied_outcome(&record, "user rejected");
    assert!(!outcome.is_error);
    assert_eq!(outcome.content, "user rejected");
}

#[test]
fn preemption_stops_blocking_local_search_when_its_async_future_is_dropped() {
    runtime().block_on(async {
        for tool in ["glob", "grep"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("sample.txt"), "needle\n").unwrap();
            let options = crate::ai_search::SearchOptions {
                query: Some("needle".into()),
                ..Default::default()
            };
            let command_root = root.path().to_path_buf();
            let (started, worker_started) = oneshot::channel();
            let (finished, worker_finished) = oneshot::channel();
            let (release, worker_release) = std::sync::mpsc::channel();
            let search = tokio::spawn(async move {
                let stopped = Arc::new(AtomicBool::new(false));
                let _guard = SearchCancellationGuard(Arc::clone(&stopped));
                tokio::task::spawn_blocking(move || {
                    started.send(()).unwrap();
                    // Hold the worker until the async search has been dropped.
                    // A blocking worker itself is not aborted by Tokio.
                    worker_release.recv_timeout(Duration::from_secs(3)).unwrap();
                    finished
                        .send(
                            crate::ai_search::search_local(&command_root, tool, &options, &stopped)
                                .map(|_| ()),
                        )
                        .unwrap();
                })
                .await
                .unwrap();
            });
            timeout(Duration::from_secs(3), worker_started)
                .await
                .unwrap()
                .unwrap();
            search.abort();
            assert!(timeout(Duration::from_secs(3), search)
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled());
            release.send(()).unwrap();
            let error = timeout(Duration::from_secs(3), worker_finished)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert_eq!(error.code, "ai_workspace_search_failed");
            assert_eq!(error.raw_message, "cancelled");
        }
    });
}

#[test]
fn preemption_terminates_local_process_tree_preserves_output_and_leaves_background_work_running() {
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let (signal, receiver) = watch::channel(false);
        let confirmed = Arc::new(AtomicBool::new(false));
        let started = Arc::new(StdMutex::new(None::<Instant>));
        let captured = Arc::new(StdMutex::new(String::new()));
        let on_chunk: OutputChunkCallback = {
            let started = started.clone();
            let captured = captured.clone();
            Arc::new(move |bytes, stream| {
                if stream != "stdout" {
                    return;
                }
                let mut content = captured.lock().unwrap();
                content.push_str(&String::from_utf8_lossy(bytes));
                if content.contains("READY") && started.lock().unwrap().is_none() {
                    *started.lock().unwrap() = Some(Instant::now());
                    signal.send_replace(true);
                }
            })
        };
        let command = if cfg!(windows) {
            // Native PowerShell argument forwarding can strip nested -Command
            // quotes. A file preserves the child script and tests a real descendant.
            let script = root.path().join("child.ps1");
            std::fs::write(&script, "Write-Output (\"CHILD:\" + $PID)\nWrite-Output READY\nStart-Sleep -Seconds 30\n").unwrap();
            format!("powershell.exe -NoProfile -NonInteractive -File '{}'", script.to_string_lossy().replace('\'', "''"))
        } else {
            "sh -c 'echo CHILD:$$; echo READY; sleep 30'".into()
        };
        let background_command = if cfg!(windows) { "Start-Sleep -Seconds 2; Write-Output BACKGROUND_DONE" } else { "sleep 2; echo BACKGROUND_DONE" };
        let (foreground, background) = tokio::join!(
            run_local_command_streaming(root.path(), &command, Duration::from_secs(20), None, Some(confirmed.clone()), Some(on_chunk), Some(receiver)),
            run_local_command_streaming(root.path(), background_command, Duration::from_secs(20), None, None, None, None),
        );
        let output = foreground.unwrap();
        assert!(confirmed.load(Ordering::SeqCst));
        assert!(started.lock().unwrap().unwrap().elapsed() < Duration::from_secs(6));
        let content = String::from_utf8_lossy(&output.stdout);
        assert!(content.contains("READY"));
        assert_eq!(output.exit_status, None);
        let pid: u32 = content.lines().find_map(|line| line.trim().strip_prefix("CHILD:")).unwrap_or_else(|| panic!("child PID missing; stdout={content}; stderr={}", String::from_utf8_lossy(&output.stderr))).parse().unwrap();
        #[cfg(windows)]
        let check = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &format!("$ErrorActionPreference='SilentlyContinue'; if (Get-Process -Id {pid}) {{ exit 1 }} else {{ exit 0 }}")]).status().await.unwrap();
        #[cfg(not(windows))]
        let check = Command::new("sh").args(["-c", &format!("! kill -0 {pid} 2>/dev/null")]).status().await.unwrap();
        assert!(check.success(), "descendant survived cancellation");
        let background = background.unwrap();
        assert_eq!(background.exit_status, Some(0));
        assert!(String::from_utf8_lossy(&background.stdout).contains("BACKGROUND_DONE"));
        let next = run_local_command_streaming(root.path(), if cfg!(windows) { "Write-Output NEXT" } else { "echo NEXT" }, Duration::from_secs(10), None, None, None, None).await.unwrap();
        assert_eq!(next.exit_status, Some(0));
        assert!(String::from_utf8_lossy(&next.stdout).contains("NEXT"));
    });
}

#[test]
fn preemption_terminates_descendant_after_parent_shell_has_already_exited() {
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let command = if cfg!(windows) {
            let script = root.path().join("orphan.ps1");
            std::fs::write(&script, "Write-Output (\"CHILD:\" + $PID)\nStart-Sleep -Seconds 1\nWrite-Output READY\nStart-Sleep -Seconds 30\n").unwrap();
            format!("Start-Process powershell.exe -NoNewWindow -ArgumentList '-NoProfile','-NonInteractive','-File','\"{}\"'; Write-Output (\"PARENT:\" + $PID)", script.to_string_lossy().replace('\'', "''"))
        } else {
            "sh -c 'echo CHILD:$$; sleep 1; echo READY; sleep 30' & echo PARENT:$$; exit 0".into()
        };
        let (signal, receiver) = watch::channel(false);
        let confirmed = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(StdMutex::new(String::new()));
        let ready = Arc::new(Notify::new());
        let on_chunk: OutputChunkCallback = {
            let captured = captured.clone();
            let ready = ready.clone();
            Arc::new(move |bytes, stream| {
                if stream == "stdout" {
                    let mut text = captured.lock().unwrap();
                    text.push_str(&String::from_utf8_lossy(bytes));
                    if text.contains("READY") { ready.notify_one(); }
                }
            })
        };
        let command_root = root.path().to_path_buf();
        let command_confirmed = confirmed.clone();
        let command_task = tokio::spawn(async move {
            run_local_command_streaming(&command_root, &command, Duration::from_secs(12), None, Some(command_confirmed), Some(on_chunk), Some(receiver)).await
        });
        tokio::time::timeout(Duration::from_secs(8), ready.notified()).await.unwrap();
        let content = captured.lock().unwrap().clone();
        let pid = |prefix: &str| -> u32 { content.lines().find_map(|line| line.trim().strip_prefix(prefix)).unwrap_or_else(|| panic!("missing {prefix}: {content}")).parse().unwrap() };
        let parent = pid("PARENT:");
        let descendant = pid("CHILD:");
        async fn is_gone(pid: u32) -> bool {
            #[cfg(windows)]
            let status = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &format!("$ErrorActionPreference='SilentlyContinue'; if (Get-Process -Id {pid}) {{ exit 1 }} else {{ exit 0 }}")]).status().await.unwrap();
            #[cfg(unix)]
            let status = Command::new("sh").args(["-c", &format!("! kill -0 {pid} 2>/dev/null")]).status().await.unwrap();
            status.success()
        }
        assert!(is_gone(parent).await, "parent still running; test must cover an exited shell");
        assert!(!is_gone(descendant).await, "descendant exited before cancellation");
        signal.send_replace(true);
        let output = tokio::time::timeout(Duration::from_secs(4), command_task).await.unwrap().unwrap().unwrap();
        assert!(confirmed.load(Ordering::SeqCst));
        assert!(String::from_utf8_lossy(&output.stdout).contains("READY"));
        assert_eq!(output.exit_status, None);
        assert!(is_gone(descendant).await, "orphan survived cancellation");
    });
}
