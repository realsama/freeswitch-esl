use std::net::SocketAddr;

use ntest::timeout;
use regex::Regex;
use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

use anyhow::Result;
use freeswitch_esl::{Esl, EslError};

async fn mock_test_server() -> Result<(JoinHandle<()>, SocketAddr)> {
    let listener = TcpListener::bind("localhost:0").await?;
    let local_address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let _ = socket.write_all(b"Content-Type: auth/request\n\n").await;

                let mut buffer = [0; 1024];
                let mut received_data = Vec::new();

                loop {
                    let n = match socket.read(&mut buffer).await {
                        Ok(0) => break, // Connection closed
                        Ok(n) => n,
                        Err(_) => break, // Error reading data
                    };
                    received_data.extend_from_slice(&buffer[0..n]);
                    // Check for two newline characters in the received data
                    while let Some(index) = received_data
                        .windows(2)
                        .position(|window| window == b"\n\n")
                    {
                        // Extract the data before the two newlines
                        let data_before_newlines = &received_data[0..index];

                        // Convert the data to a string for comparison
                        let mut data_string =
                            String::from_utf8_lossy(data_before_newlines).to_string();

                        // HACK
                        let response_text: Vec<String> = if data_string.starts_with("bgapi")
                            && data_string.contains("Job-UUID")
                        {
                            let re =
                                Regex::new(r"(?P<bgapi>.+)\nJob-UUID: (?P<uuid>[0-9a-fA-F-]+)")
                                    .unwrap();
                            let captures = re.captures(&data_string).unwrap();
                            // Extract components
                            let _ = &captures["bgapi"];
                            let uuid_old = &captures["uuid"];
                            let uuid_old = uuid_old.to_owned();

                            let new_uuids = uuid::Uuid::new_v4().to_string();
                            data_string = data_string.replace(&uuid_old, &new_uuids);
                            let reloadxml_app = format!("bgapi reloadxml\nJob-UUID: {}", new_uuids);
                            let some_user_that_doesnt_exists = format!("bgapi originate user/some_user_that_doesnt_exists karan\nJob-UUID: {}",new_uuids);
                            let never_respond =
                                format!("bgapi never_respond\nJob-UUID: {}", new_uuids);
                            let silent = format!("bgapi silent\nJob-UUID: {}", new_uuids);
                            let cancel_then_complete = format!(
                                "bgapi cancel_then_complete\nJob-UUID: {}",
                                new_uuids
                            );

                            if data_string == reloadxml_app {
                                let first_1 = "Content-Type: command/reply\nReply-Text: +OK Job-UUID: UUID_PLACEHOLDER\nJob-UUID: UUID_PLACEHOLDER\n\n";
                                let second_1 = "Content-Length: 615\nContent-Type: text/event-json\n\n{\"Event-Name\":\"BACKGROUND_JOB\",\"Core-UUID\":\"bd0e8916-6a60-4e11-8978-db8580b440a6\",\"FreeSWITCH-Hostname\":\"ip-172-31-32-63\",\"FreeSWITCH-Switchname\":\"ip-172-31-32-63\",\"FreeSWITCH-IPv4\":\"172.31.32.63\",\"FreeSWITCH-IPv6\":\"::1\",\"Event-Date-Local\":\"2023-09-12 04:31:37\",\"Event-Date-GMT\":\"Tue, 12 Sep 2023 04:31:37 GMT\",\"Event-Date-Timestamp\":\"1694493097638660\",\"Event-Calling-File\":\"mod_event_socket.c\",\"Event-Calling-Function\":\"api_exec\",\"Event-Calling-Line-Number\":\"1572\",\"Event-Sequence\":\"18546\",\"Job-UUID\":\"UUID_PLACEHOLDER\",\"Job-Command\":\"reloadxml\",\"Content-Length\":\"14\",\"_body\":\"+OK [Success]\\n\"}";
                                let first = first_1.replace("UUID_PLACEHOLDER", &uuid_old);
                                let second = second_1.replace("UUID_PLACEHOLDER", &uuid_old);
                                vec![first, second]
                            } else if data_string == some_user_that_doesnt_exists {
                                let first_1 = "Content-Type: command/reply\nReply-Text: +OK Job-UUID: UUID_PLACEHOLDER\nJob-UUID: UUID_PLACEHOLDER\n\n";
                                let second_1 = "Content-Length: 684\nContent-Type: text/event-json\n\n{\"Event-Name\":\"BACKGROUND_JOB\",\"Core-UUID\":\"bd0e8916-6a60-4e11-8978-db8580b440a6\",\"FreeSWITCH-Hostname\":\"ip-172-31-32-63\",\"FreeSWITCH-Switchname\":\"ip-172-31-32-63\",\"FreeSWITCH-IPv4\":\"172.31.32.63\",\"FreeSWITCH-IPv6\":\"::1\",\"Event-Date-Local\":\"2023-09-13 06:56:24\",\"Event-Date-GMT\":\"Wed, 13 Sep 2023 06:56:24 GMT\",\"Event-Date-Timestamp\":\"1694588184538697\",\"Event-Calling-File\":\"mod_event_socket.c\",\"Event-Calling-Function\":\"api_exec\",\"Event-Calling-Line-Number\":\"1572\",\"Event-Sequence\":\"29999\",\"Job-UUID\":\"UUID_PLACEHOLDER\",\"Job-Command\":\"originate\",\"Job-Command-Arg\":\"user/some_user_that_doesnt_exists karan\",\"Content-Length\":\"23\",\"_body\":\"-ERR SUBSCRIBER_ABSENT\\n\"}";
                                let first = first_1.replace("UUID_PLACEHOLDER", &uuid_old);
                                let second = second_1.replace("UUID_PLACEHOLDER", &uuid_old);
                                vec![first, second]
                            } else if data_string == never_respond {
                                let first_1 = "Content-Type: command/reply\nReply-Text: +OK Job-UUID: UUID_PLACEHOLDER\nJob-UUID: UUID_PLACEHOLDER\n\n";
                                let first = first_1.replace("UUID_PLACEHOLDER", &uuid_old);
                                vec![first]
                            } else if data_string == silent {
                                // Drain without responding — simulates a bgapi
                                // where the command/reply frame never arrives.
                                vec![]
                            } else if data_string == cancel_then_complete {
                                // Command-reply lands immediately so the
                                // client's bgapi() progresses to awaiting the
                                // BACKGROUND_JOB. The delay lets the test
                                // abort the bgapi future before the second
                                // frame arrives, exercising the reader's
                                // dropped-waiter branch.
                                let first_1 = "Content-Type: command/reply\nReply-Text: +OK Job-UUID: UUID_PLACEHOLDER\nJob-UUID: UUID_PLACEHOLDER\n\n";
                                let second_1 = "Content-Length: 615\nContent-Type: text/event-json\n\n{\"Event-Name\":\"BACKGROUND_JOB\",\"Core-UUID\":\"bd0e8916-6a60-4e11-8978-db8580b440a6\",\"FreeSWITCH-Hostname\":\"ip-172-31-32-63\",\"FreeSWITCH-Switchname\":\"ip-172-31-32-63\",\"FreeSWITCH-IPv4\":\"172.31.32.63\",\"FreeSWITCH-IPv6\":\"::1\",\"Event-Date-Local\":\"2023-09-12 04:31:37\",\"Event-Date-GMT\":\"Tue, 12 Sep 2023 04:31:37 GMT\",\"Event-Date-Timestamp\":\"1694493097638660\",\"Event-Calling-File\":\"mod_event_socket.c\",\"Event-Calling-Function\":\"api_exec\",\"Event-Calling-Line-Number\":\"1572\",\"Event-Sequence\":\"18546\",\"Job-UUID\":\"UUID_PLACEHOLDER\",\"Job-Command\":\"reloadxml\",\"Content-Length\":\"14\",\"_body\":\"+OK [Success]\\n\"}";
                                let first =
                                    first_1.replace("UUID_PLACEHOLDER", &uuid_old);
                                let second =
                                    second_1.replace("UUID_PLACEHOLDER", &uuid_old);
                                if socket.write_all(first.as_bytes()).await.is_err() {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(60)).await;
                                if socket.write_all(second.as_bytes()).await.is_err() {
                                    break;
                                }
                                vec![]
                            } else {
                                panic!("Unhandled application")
                            }
                        } else {
                            // data_string.contains("Job-UUID")

                            if data_string == "api never_respond" {
                                received_data.drain(0..=index + 1);
                                continue;
                            }

                            if data_string == "api disconnect_me" {
                                // Return from the per-connection task so the
                                // socket drops and the client's reader sees
                                // EOF.
                                return;
                            }

                            if data_string == "api trigger_malformed" {
                                // Emit a malformed text/event-json body followed
                                // by a real api/response. The reader must warn
                                // and continue, not panic, or the api() call
                                // below it will never receive its reply.
                                let malformed =
                                    "Content-Length: 10\nContent-Type: text/event-json\n\nnot-json!!";
                                let reply =
                                    "Content-Type: api/response\nContent-Length: 14\n\n+OK [Success]\n\n";
                                if socket.write_all(malformed.as_bytes()).await.is_err() {
                                    break;
                                }
                                if socket.write_all(reply.as_bytes()).await.is_err() {
                                    break;
                                }
                                received_data.drain(0..=index + 1);
                                continue;
                            }

                            let response_text = match data_string.as_ref() {
                            "auth ClueCon" => {
                                "Content-Type: command/reply\nReply-Text: +OK accepted\n\n"
                            }
                            "auth ClueCons"=>{
                                "Content-Type: command/reply\nReply-Text: -ERR invalid\n\n"
                            }
                            "api reloadxml" => {
                                "Content-Type: api/response\nContent-Length: 14\n\n+OK [Success]\n\n"
                            }
                            "api sofia profile external restart" => {
                                "Content-Type: api/response\nContent-Length: 41\n\nReload XML [Success]\nrestarting: external"
                            }
                            "api originate {origination_uuid=karan}loopback/1000 &conference(karan)" => {
                                "Content-Type: api/response\nContent-Length: 10\n\n+OK karan\n"
                            }
                            "api uuid_kill karan" => {
                                "Content-Type: api/response\nContent-Length: 4\n\n+OK\n"
                            }
                            "bgapi never_respond" => {
                                "Content-Type: command/reply\nReply-Text: +OK Job-UUID: 14f61274-6487-4b79-b97b-ee0feca07e86\nJob-UUID: 14f61274-6487-4b79-b97b-ee0feca07e86\n\n"
                            }
                            "event json BACKGROUND_JOB CHANNEL_EXECUTE_COMPLETE"=>{
                                "Content-Type: command/reply\nReply-Text: +OK event listener enabled json\n\n"
                            }
                            "api originate user/some_user_that_doesnt_exists karan"=>{
                                "Content-Type: api/response\nContent-Length: 23\n\n-ERR SUBSCRIBER_ABSENT\n\n"
                            },
                            "bgapi reloadxml"=>{
                                "Content-Type: command/reply\nReply-Text: +OK Job-UUID: 14f61274-6487-4b79-b97b-ee0feca07e86\nJob-UUID: 14f61274-6487-4b79-b97b-ee0feca07e86\n\nContent-Length: 615\nContent-Type: text/event-json\n\n{\"Event-Name\":\"BACKGROUND_JOB\",\"Core-UUID\":\"bd0e8916-6a60-4e11-8978-db8580b440a6\",\"FreeSWITCH-Hostname\":\"ip-172-31-32-63\",\"FreeSWITCH-Switchname\":\"ip-172-31-32-63\",\"FreeSWITCH-IPv4\":\"172.31.32.63\",\"FreeSWITCH-IPv6\":\"::1\",\"Event-Date-Local\":\"2023-09-13 06:34:46\",\"Event-Date-GMT\":\"Wed, 13 Sep 2023 06:34:46 GMT\",\"Event-Date-Timestamp\":\"1694586886798662\",\"Event-Calling-File\":\"mod_event_socket.c\",\"Event-Calling-Function\":\"api_exec\",\"Event-Calling-Line-Number\":\"1572\",\"Event-Sequence\":\"29837\",\"Job-UUID\":\"14f61274-6487-4b79-b97b-ee0feca07e86\",\"Job-Command\":\"reloadxml\",\"Content-Length\":\"14\",\"_body\":\"+OK [Success]\\n\"}"
                            },
                            _ => {
                                "Content-Type: command/reply\nReply-Text: -ERR command not found\n\n"
                            }
                        };
                            vec![response_text.to_string()]
                        };
                        let response_text = response_text.iter();
                        for response in response_text {
                            if socket.write_all(response.as_bytes()).await.is_err() {
                                eprintln!("error writing data");
                                break; // Error writing data
                            }
                        }

                        // Remove the processed data from the received_data buffer
                        received_data.drain(0..=index + 1);
                    }
                }
            });
        }
    });
    Ok((server, local_address))
}
#[tokio::test]
#[timeout(1000)]
async fn reloadxml() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    let response = inbound.api("reloadxml").await;
    assert_eq!(Ok("[Success]".into()), response);
    Ok(())
}
#[tokio::test]
#[timeout(10000)]
async fn reloadxml_with_bgapi() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    let response = inbound.bgapi("reloadxml").await;
    assert_eq!(Ok("[Success]".into()), response);
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn call_user_that_doesnt_exists() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    let response = inbound
        .api("originate user/some_user_that_doesnt_exists karan")
        .await
        .unwrap_err();
    assert_eq!(EslError::ApiError("SUBSCRIBER_ABSENT".into()), response);
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn send_recv_test() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    let response = inbound.send_recv(b"api reloadxml").await?;
    let body = response.body().clone().unwrap();
    assert_eq!("+OK [Success]\n", body);
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn send_recv_timeout_marks_connection_closed() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;

    let response = inbound
        .send_recv_timeout(b"api never_respond", Duration::from_millis(25))
        .await;

    assert_eq!(Err(EslError::Timeout(Duration::from_millis(25))), response);
    assert!(!inbound.connected());

    let response = inbound.api("reloadxml").await;
    assert_eq!(Err(EslError::ConnectionClosed), response);
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn api_timeout_marks_connection_closed_on_elapse() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;

    let response = inbound
        .api_timeout("never_respond", Duration::from_millis(25))
        .await;

    assert_eq!(Err(EslError::Timeout(Duration::from_millis(25))), response);
    assert!(!inbound.connected());

    let response = inbound.api("reloadxml").await;
    assert_eq!(Err(EslError::ConnectionClosed), response);
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn cancelled_send_recv_timeout_still_fails_connection() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    let task_connection = inbound.clone();

    let handle = tokio::spawn(async move {
        task_connection
            .send_recv_timeout(b"api never_respond", Duration::from_millis(25))
            .await
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    handle.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(!inbound.connected());
    let response = inbound.api("reloadxml").await;
    assert_eq!(Err(EslError::ConnectionClosed), response);
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn bgapi_timeout_reply_timeout_elapse_marks_connection_failed() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;

    let response = inbound
        .bgapi_timeout(
            "silent",
            Duration::from_millis(25),
            Duration::from_secs(1),
        )
        .await;

    assert_eq!(Err(EslError::Timeout(Duration::from_millis(25))), response);
    assert!(!inbound.connected());

    let response = inbound.api("reloadxml").await;
    assert_eq!(Err(EslError::ConnectionClosed), response);
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn bgapi_timeout_only_times_out_background_job() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;

    let response = inbound
        .bgapi_timeout(
            "never_respond",
            Duration::from_secs(1),
            Duration::from_millis(25),
        )
        .await;

    assert_eq!(Err(EslError::Timeout(Duration::from_millis(25))), response);
    assert!(inbound.connected());

    let response = inbound.api("reloadxml").await;
    assert_eq!(Ok("[Success]".into()), response);
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn cancelled_bgapi_timeout_still_cleans_background_job() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    let task_connection = inbound.clone();

    let handle = tokio::spawn(async move {
        task_connection
            .bgapi_timeout(
                "never_respond",
                Duration::from_secs(1),
                Duration::from_millis(25),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    handle.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(inbound.connected());
    let response = inbound.api("reloadxml").await;
    assert_eq!(Ok("[Success]".into()), response);
    Ok(())
}

#[tokio::test]
#[timeout(5000)]
async fn reader_survives_dropped_bgapi_waiter() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;

    let task_conn = inbound.clone();
    let handle =
        tokio::spawn(async move { task_conn.bgapi("cancel_then_complete").await });
    // Let the command-reply land so the bgapi future is awaiting the
    // BACKGROUND_JOB, then abort. rx is dropped; tx stays in the map.
    tokio::time::sleep(Duration::from_millis(30)).await;
    handle.abort();
    // Let the server emit the BACKGROUND_JOB targeting the dropped waiter.
    tokio::time::sleep(Duration::from_millis(120)).await;

    // Reader must still be alive — a subsequent api call still works.
    let response = inbound.api("reloadxml").await;
    assert_eq!(Ok("[Success]".into()), response);
    assert!(inbound.connected());
    Ok(())
}

#[tokio::test]
#[timeout(5000)]
async fn reader_exit_clears_pending_waiters() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;

    // Pending bgapi: server sends the command-reply but never emits
    // BACKGROUND_JOB, so this blocks in the background_jobs map.
    let bgapi_conn = inbound.clone();
    let bgapi_handle =
        tokio::spawn(async move { bgapi_conn.bgapi("never_respond").await });
    // Pending command: server drains without replying, so this blocks in
    // the commands FIFO.
    let api_conn = inbound.clone();
    let api_handle =
        tokio::spawn(async move { api_conn.send_recv(b"api never_respond").await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Trigger server-side disconnect. Reader sees EOF and must clear every
    // pending waiter, not just the one it happens to be holding.
    let _ = inbound.send_recv(b"api disconnect_me").await;

    let bgapi_result = bgapi_handle.await.unwrap();
    let api_result = api_handle.await.unwrap();
    assert!(matches!(bgapi_result, Err(EslError::ConnectionClosed)));
    assert!(matches!(api_result, Err(EslError::ConnectionClosed)));
    assert!(!inbound.connected());
    Ok(())
}

#[tokio::test]
#[timeout(5000)]
async fn reader_survives_malformed_event_json_body() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;

    let response = inbound.api("trigger_malformed").await;
    assert_eq!(Ok("[Success]".into()), response);

    let response = inbound.api("reloadxml").await;
    assert_eq!(Ok("[Success]".into()), response);
    assert!(inbound.connected());
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn wrong_password() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCons").await;
    assert_eq!(EslError::AuthFailed, inbound.unwrap_err());
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn multiple_actions() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    let body = inbound.bgapi("reloadxml").await;
    assert_eq!(Ok("[Success]".into()), body);
    let body = inbound
        .bgapi("originate user/some_user_that_doesnt_exists karan")
        .await;
    assert_eq!(
        Err(EslError::ApiError("SUBSCRIBER_ABSENT".to_string())),
        body
    );
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn concurrent_api() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    let response1 = inbound.api("reloadxml");
    let response2 = inbound.api("originate user/some_user_that_doesnt_exists karan");
    let response3 = inbound.api("reloadxml");
    let (response1, response2, response3) = tokio::join!(response1, response2, response3);
    assert_eq!(Ok("[Success]".into()), response1);
    assert_eq!(
        Err(EslError::ApiError("SUBSCRIBER_ABSENT".into())),
        response2
    );
    assert_eq!(Ok("[Success]".into()), response3);
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn concurrent_bgapi() -> Result<()> {
    let (_, addr) = mock_test_server().await?;

    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    let response1 = inbound.bgapi("reloadxml");
    let response2 = inbound.bgapi("originate user/some_user_that_doesnt_exists karan");
    let response3 = inbound.bgapi("reloadxml");
    let (response1, response2, response3) = tokio::join!(response1, response2, response3);
    assert_eq!(Ok("[Success]".to_string()), response1);
    assert_eq!(
        Err(EslError::ApiError("SUBSCRIBER_ABSENT".to_string())),
        response2
    );
    assert_eq!(Ok("[Success]".to_string()), response3);
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn connected_status() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    assert!(inbound.connected());
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn restart_external_profile() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;
    let body = inbound.api("sofia profile external restart").await;
    assert_eq!(
        Ok("Reload XML [Success]\nrestarting: external".into()),
        body
    );
    Ok(())
}

#[tokio::test]
#[timeout(10000)]
async fn uuid_kill() -> Result<()> {
    let (_, addr) = mock_test_server().await?;
    let stream = TcpStream::connect(addr).await?;
    let inbound = Esl::inbound(stream, "ClueCon").await?;

    let uuid = inbound
        .api("originate {origination_uuid=karan}loopback/1000 &conference(karan)")
        .await?;
    assert_eq!("karan", uuid);
    let uuid_kill_response = inbound.api("uuid_kill karan").await?;
    assert_eq!("", uuid_kill_response);
    Ok(())
}
