use super::*;
use std::io::Write;

fn cases() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../test-support/update-channel-cases.json"
    ))
    .unwrap()
}
fn response(body: &[u8], headers: &str) -> Vec<u8> {
    let mut result = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    result.extend(body);
    result
}
fn server(
    build: impl FnOnce(&str) -> Vec<Vec<u8>>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}/releases", listener.local_addr().unwrap());
    let responses = build(&base);
    let thread = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing fixture request");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                assert!(request.len() < 8192);
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            requests.push(String::from_utf8(request).unwrap());
            let _ = socket.write_all(&response);
        }
        requests
    });
    (base, thread)
}
#[test]
fn shared_node_rust_selection_cases() {
    for case in cases()["cases"].as_array().unwrap() {
        let releases: Vec<Release> = serde_json::from_value(case["releases"].clone()).unwrap();
        let selected = select(&releases, case["target"].as_str().unwrap());
        if case.get("expectedError").is_some() {
            assert!(selected.is_err(), "{}: {selected:?}", case["name"]);
        } else {
            assert_eq!(
                selected.unwrap().tag(),
                case["expectedTag"].as_str().unwrap(),
                "{}",
                case["name"]
            );
        }
    }
}
#[test]
fn pagination_rejects_unpinned_jumps_skips_caps_and_ambiguity() {
    let base = RELEASES_API;
    assert!(
        next_page(
            Some(&format!("<{}>; rel=\"next\"", page_url(base, 2))),
            base,
            1
        )
        .unwrap()
    );
    for link in [
        "<https://evil.invalid/releases?per_page=100&page=2>; rel=\"next\"".to_string(),
        format!("<{}>; rel=\"next\"", page_url(base, 3)),
        format!("<{}>; rel=\"last\"", page_url(base, 3)),
        format!("<{}>; rel=\"next\"", page_url(base, 11)),
        format!(
            "<{}>; rel=\"next\", <{}>; rel=\"next\"",
            page_url(base, 2),
            page_url(base, 2)
        ),
    ] {
        assert!(next_page(Some(&link), base, 1).is_err(), "{link}");
    }
}
#[test]
fn complete_inventory_uses_all_pages_and_rechecks_head_without_auth() {
    let fixture = cases();
    let releases = fixture["cases"][0]["releases"].as_array().unwrap();
    let first = serde_json::to_vec(&vec![&releases[0]]).unwrap();
    let second = serde_json::to_vec(&vec![&releases[1]]).unwrap();
    let (base, thread) = server(|base| {
        let header = format!(
            "Link: <{}>; rel=\"next\", <{}>; rel=\"last\"\r\n",
            page_url(base, 2),
            page_url(base, 2)
        );
        vec![
            response(&first, &header),
            response(&second, ""),
            response(&first, &header),
        ]
    });
    let selected = discover(
        &client().unwrap(),
        &base,
        "windows-x64",
        Instant::now() + DISCOVERY_TIMEOUT,
    )
    .unwrap();
    assert_eq!(selected.tag(), "v2.6.0");
    let requests = thread.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /releases?per_page=100&page=1 HTTP/1.1"));
    assert!(requests[1].starts_with("GET /releases?per_page=100&page=2 HTTP/1.1"));
    assert!(
        requests
            .iter()
            .all(|r| !r.to_lowercase().contains("authorization:")
                && !r.to_lowercase().contains("cookie:"))
    );
}
#[test]
fn moved_inventory_is_an_error_not_an_older_success() {
    let fixture = cases();
    let releases = fixture["cases"][0]["releases"].as_array().unwrap();
    let first = serde_json::to_vec(&vec![&releases[0]]).unwrap();
    let second = serde_json::to_vec(&vec![&releases[1]]).unwrap();
    let (base, thread) = server(|base| {
        let header = format!("Link: <{}>; rel=\"next\"\r\n", page_url(base, 2));
        vec![
            response(&first, &header),
            response(&second, ""),
            response(&second, &header),
        ]
    });
    assert!(
        discover(
            &client().unwrap(),
            &base,
            "windows-x64",
            Instant::now() + DISCOVERY_TIMEOUT
        )
        .unwrap_err()
        .contains("changed")
    );
    thread.join().unwrap();
}
#[test]
fn api_redirect_is_rejected_before_any_redirected_request() {
    let (base, thread) = server(|base| {
        vec![format!(
        "HTTP/1.1 302 Found\r\nLocation: {base}/unexpected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    ).into_bytes()]
    });
    let error = discover(
        &client().unwrap(),
        &base,
        "windows-x64",
        Instant::now() + DISCOVERY_TIMEOUT,
    )
    .unwrap_err();
    assert!(error.contains("302"));
    assert_eq!(thread.join().unwrap().len(), 1);
}
#[test]
fn inventory_body_bytes_deadline_and_record_counts_are_bounded() {
    for headers in [
        format!("Content-Length: {}\r\n", PAGE_BYTES + 1),
        "".to_string(),
    ] {
        let body = if headers.is_empty() {
            vec![b' '; PAGE_BYTES + 1]
        } else {
            vec![]
        };
        let (base, thread) = server(|_| vec![response(&body, &headers)]);
        assert!(
            discover(
                &client().unwrap(),
                &base,
                "windows-x64",
                Instant::now() + DISCOVERY_TIMEOUT
            )
            .unwrap_err()
            .contains("size limit")
        );
        thread.join().unwrap();
    }
    assert!(
        discover(
            &client().unwrap(),
            "http://127.0.0.1:1",
            "windows-x64",
            Instant::now() - Duration::from_millis(1)
        )
        .unwrap_err()
        .contains("time limit")
    );
    let first = cases()["cases"][0]["releases"][0].clone();
    let body = serde_json::to_vec(&vec![first; PAGE_SIZE + 1]).unwrap();
    let (base, thread) = server(|_| vec![response(&body, "")]);
    assert!(
        discover(
            &client().unwrap(),
            &base,
            "windows-x64",
            Instant::now() + DISCOVERY_TIMEOUT
        )
        .unwrap_err()
        .contains("too many")
    );
    thread.join().unwrap();
    let (base, thread) = server(|_| vec![response(b"[]", "")]);
    let mut bytes = INVENTORY_BYTES;
    assert!(
        fetch_page(
            &client().unwrap(),
            &base,
            1,
            Instant::now() + DISCOVERY_TIMEOUT,
            &mut bytes
        )
        .unwrap_err()
        .contains("size limit")
    );
    thread.join().unwrap();
}
#[test]
fn rate_limit_cooldown_is_safe_and_independent_of_success_preferences() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("retry-after", "120".parse().unwrap());
    assert!(rate_limit_error(&headers).contains("120 seconds"));
    assert!(
        check_cooldown()
            .unwrap_err()
            .contains("temporarily limited")
    );
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("prefs.json");
    super::super::update_prefs_at(&path, |prefs| {
        prefs.highest_trusted_version = Some("1.0.0".to_string());
        Ok(())
    })
    .unwrap();
    let original = std::fs::read(&path).unwrap();
    let error = super::super::check_for_update_at(&path, true, || {
        check_cooldown()?;
        panic!("forced check bypassed cooldown");
    })
    .err()
    .unwrap();
    let public = super::super::UpdatePublicError::check_failed(error);
    assert_eq!(public.code, "update-rate-limited");
    assert_eq!(public.action, "waitAndRetry");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    *RATE_LIMIT_UNTIL.lock().unwrap() = None;
}

#[test]
fn ten_full_pages_are_unavailable_even_without_a_final_next_link() {
    let record = cases()["cases"][0]["releases"][0].clone();
    let body = serde_json::to_vec(&vec![record; PAGE_SIZE]).unwrap();
    let (base, thread) = server(|base| {
        (1..=MAX_PAGES)
            .map(|page| {
                let header = if page < MAX_PAGES {
                    format!("Link: <{}>; rel=\"next\"\r\n", page_url(base, page + 1))
                } else {
                    String::new()
                };
                response(&body, &header)
            })
            .collect()
    });
    let error = discover(
        &client().unwrap(),
        &base,
        "windows-x64",
        Instant::now() + DISCOVERY_TIMEOUT,
    )
    .unwrap_err();
    assert!(error.contains("Download a newer portable app manually"));
    assert_eq!(thread.join().unwrap().len(), MAX_PAGES);
}

#[test]
fn advertised_empty_continuation_cannot_select_an_older_release() {
    let record = cases()["cases"][0]["releases"][0].clone();
    let first = serde_json::to_vec(&vec![record]).unwrap();
    let (base, thread) = server(|base| {
        let header = format!("Link: <{}>; rel=\"next\"\r\n", page_url(base, 2));
        // The advertised continuation is empty and has no Link. There must be
        // no third request/head check and no successful selection from page 1.
        vec![response(&first, &header), response(b"[]", "")]
    });
    let error = discover(
        &client().unwrap(),
        &base,
        "windows-x64",
        Instant::now() + DISCOVERY_TIMEOUT,
    )
    .unwrap_err();
    assert!(error.contains("Incomplete"));
    assert_eq!(thread.join().unwrap().len(), 2);
    let (base, thread) = server(|_| vec![response(b"[]", "")]);
    assert!(
        discover(
            &client().unwrap(),
            &base,
            "windows-x64",
            Instant::now() + DISCOVERY_TIMEOUT
        )
        .unwrap_err()
        .contains("No stable")
    );
    thread.join().unwrap();
}
