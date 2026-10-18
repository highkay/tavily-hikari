    // Pinned-session key migration (plan provider-key-429-retry todo 13).
    //
    // `include!`d from `mcp_rebalance_and_follow_up.rs` because that file sits at
    // its `rust_source_line_budgets` ceiling (3036/3050) and the plan forbids
    // raising a budget. The include keeps the module path - and with it the CI
    // manifest prefix `server::tests::mcp_rebalance_and_follow_up::mcp_follow_up_`
    // - unchanged, so no manifest edit is needed either. rustfmt never visits an
    // included fragment, so the four-space module indentation is kept by hand.
    //
    // What these cases pin: an AuthFailed or Exhausted rejection kills the KEY a
    // pinned session rides, not the session, so the ladder migrates it (strict
    // acquire with the dead key excluded -> one proxy-driven re-`initialize` ->
    // one atomic rebind -> one replay) and the client only ever sees a success
    // under its STABLE proxy session id. When the re-`initialize` is refused the
    // migration aborts: the original rejection is served byte-for-byte, the
    // session row keeps its old binding, and the failing request costs at most
    // TWO upstream calls - no replay, no loop. The 429 ladder is untouched and
    // stays pinned by `mcp_follow_up_retries_once_after_retry_after_and_keeps_session_pinned`.

    /// The pinned session's upstream binding as the DB sees it, plus whether the
    /// row is still live.
    async fn fetch_mcp_session_upstream_binding(
        pool: &sqlx::SqlitePool,
        proxy_session_id: &str,
    ) -> (Option<String>, Option<String>, Option<i64>) {
        sqlx::query_as(
            r#"
            SELECT upstream_key_id, upstream_session_id, revoked_at
            FROM mcp_sessions
            WHERE proxy_session_id = ?
            LIMIT 1
            "#,
        )
        .bind(proxy_session_id)
        .fetch_one(pool)
        .await
        .expect("fetch pinned mcp session binding")
    }

    /// `api_keys.id` of one seeded secret: it links the key VALUE the mock saw
    /// to the key ID the session row names.
    async fn fetch_mcp_migration_key_id(pool: &sqlx::SqlitePool, secret: &str) -> String {
        sqlx::query_scalar("SELECT id FROM api_keys WHERE api_key = ?")
            .bind(secret)
            .fetch_one(pool)
            .await
            .expect("fetch api key id by secret")
    }

    /// Everything one migration case asserts on, produced by the shared driver.
    struct McpKeyMigrationOutcome {
        proxy_session_id: String,
        /// Status of the follow-up the client sent on the pinned (dead) key.
        follow_up_status: StatusCode,
        /// Raw bytes of that follow-up response - the byte-for-byte passthrough
        /// evidence when the migration aborts.
        follow_up_body: Bytes,
        /// Tool count of that response, when it was a successful `tools/list`.
        follow_up_tools: Option<usize>,
        /// Second follow-up on the SAME proxy session id, sent only when the
        /// migration was expected to succeed: proves the migrated binding is
        /// usable and the proxy session id is stable.
        second_follow_up_status: Option<StatusCode>,
        second_follow_up_tools: Option<usize>,
        /// Every upstream call in order.
        calls: Vec<KeyMigrationSessionCall>,
        /// Pooled key the session was pinned to (the one the mock rejects).
        pinned_key: String,
        /// The other pooled key - the migration target.
        migrated_key: String,
        db_str: String,
        db_path: PathBuf,
    }

    impl McpKeyMigrationOutcome {
        fn calls_for(&self, method: &str) -> Vec<&KeyMigrationSessionCall> {
            self.calls
                .iter()
                .filter(|call| call.method == method)
                .collect()
        }
    }

    /// Shared driver: initialize a pinned upstream-mode session, send ONE
    /// `tools/list` follow-up into the ladder, and (unless the case expects the
    /// migration to abort) send a second follow-up on the same proxy session id.
    async fn run_mcp_key_migration_scenario(
        prefix: &str,
        rejection: McpKeyMigrationRejection,
        reject_reinitialize: bool,
    ) -> McpKeyMigrationOutcome {
        let db_path = temp_db_path(prefix);
        let db_str = db_path.to_string_lossy().to_string();
        let first_api_key = format!("tvly-{prefix}-a");
        let second_api_key = format!("tvly-{prefix}-b");
        let pool_keys = vec![first_api_key.clone(), second_api_key.clone()];
        let (upstream_addr, calls) = spawn_mock_mcp_upstream_for_session_key_migration(
            pool_keys,
            rejection,
            reject_reinitialize,
        )
        .await;
        let upstream = format!("http://{}", upstream_addr);

        let proxy = TavilyProxy::with_endpoint(
            vec![first_api_key.clone(), second_api_key.clone()],
            &upstream,
            &db_str,
        )
        .await
        .expect("proxy created");
        let access_token = proxy
            .create_access_token(Some(prefix))
            .await
            .expect("create access token");
        let proxy_addr = spawn_proxy_server(proxy.clone(), upstream.clone()).await;
        let url = format!(
            "http://{}/mcp?tavilyApiKey={}",
            proxy_addr, access_token.token
        );
        let client = Client::new();

        let initialize = client
            .post(&url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", "2025-03-26")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": "init-migration",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "browser-probe", "version": "0.1.0" }
                }
            }))
            .send()
            .await
            .expect("initialize request");
        assert!(
            initialize.status().is_success(),
            "session initialize should succeed"
        );
        let proxy_session_id = initialize
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .expect("initialize response should expose mcp-session-id")
            .to_string();

        let follow_up = client
            .post(&url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", "2025-03-26")
            .header("mcp-session-id", proxy_session_id.as_str())
            .json(&json!({
                "jsonrpc": "2.0",
                "id": "tools-migration",
                "method": "tools/list"
            }))
            .send()
            .await
            .expect("follow-up request");
        let follow_up_status = follow_up.status();
        let follow_up_body = follow_up.bytes().await.expect("follow-up body");
        let follow_up_tools = serde_json::from_slice::<Value>(&follow_up_body)
            .ok()
            .and_then(|body| body["result"]["tools"].as_array().map(Vec::len));

        let mut second_follow_up_status = None;
        let mut second_follow_up_tools = None;
        if !reject_reinitialize {
            let second = client
                .post(&url)
                .header("accept", "application/json, text/event-stream")
                .header("content-type", "application/json")
                .header("mcp-protocol-version", "2025-03-26")
                .header("mcp-session-id", proxy_session_id.as_str())
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": "tools-after-migration",
                    "method": "tools/list"
                }))
                .send()
                .await
                .expect("second follow-up request");
            second_follow_up_status = Some(second.status());
            let body = second.bytes().await.expect("second follow-up body");
            second_follow_up_tools = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|body| body["result"]["tools"].as_array().map(Vec::len));
        }

        let recorded = calls
            .lock()
            .expect("key migration calls lock poisoned")
            .clone();
        let pinned_key = recorded
            .first()
            .expect("the session initialize should reach the mock")
            .tavily_api_key
            .clone();
        assert_eq!(
            recorded.first().map(|call| call.method.as_str()),
            Some("initialize"),
            "the first upstream call is the session initialize"
        );
        let migrated_key = if pinned_key == first_api_key {
            second_api_key
        } else {
            first_api_key
        };

        McpKeyMigrationOutcome {
            proxy_session_id,
            follow_up_status,
            follow_up_body,
            follow_up_tools,
            second_follow_up_status,
            second_follow_up_tools,
            calls: recorded,
            pinned_key,
            migrated_key,
            db_str,
            db_path,
        }
    }

    /// Shared tail of the two success cases: the session row moved to the new
    /// key AND the new upstream session id, stayed live, and the client's next
    /// call on the SAME proxy session id is served by the migrated key.
    async fn assert_mcp_session_migrated(outcome: &McpKeyMigrationOutcome) {
        assert_eq!(
            outcome.follow_up_status,
            StatusCode::OK,
            "the client only ever sees the migrated success"
        );
        assert_eq!(outcome.follow_up_tools, Some(1));
        assert_eq!(
            outcome.second_follow_up_status,
            Some(StatusCode::OK),
            "the stable proxy session id keeps working after the migration"
        );
        assert_eq!(outcome.second_follow_up_tools, Some(1));

        // Upstream call chain: pin, reject, re-initialize, replay, and the
        // client's NEXT follow-up on the same proxy session id. Exactly ONE
        // replay of the rejected call, so no loop.
        assert_eq!(
            outcome.calls.len(),
            5,
            "unexpected upstream call chain: {:?}",
            outcome.calls
        );
        assert_eq!(
            outcome.calls_for("initialize").len(),
            2,
            "one pinning initialize plus exactly one re-initialize"
        );
        assert_eq!(
            outcome.calls_for("tools/list").len(),
            3,
            "the rejected call + ONE replay + the client's next follow-up"
        );
        assert_eq!(outcome.calls[0].method, "initialize");
        assert_eq!(outcome.calls[0].tavily_api_key, outcome.pinned_key);
        assert_eq!(outcome.calls[1].method, "tools/list");
        assert_eq!(outcome.calls[1].tavily_api_key, outcome.pinned_key);
        assert_eq!(
            outcome.calls[1].upstream_session_id_header.as_deref(),
            Some(MCP_KEY_MIGRATION_PINNED_SESSION),
            "the rejected follow-up still rode the original upstream session"
        );
        let rebind_initialize = &outcome.calls[2];
        assert_eq!(rebind_initialize.method, "initialize");
        assert_eq!(
            rebind_initialize.tavily_api_key, outcome.migrated_key,
            "the re-initialize runs on the newly acquired key"
        );
        assert_eq!(
            rebind_initialize.upstream_session_id_header, None,
            "the re-initialize must open a NEW upstream session, not reuse the dead one"
        );
        let replay = &outcome.calls[3];
        assert_eq!(replay.method, "tools/list");
        assert_eq!(replay.tavily_api_key, outcome.migrated_key);
        assert_eq!(
            replay.upstream_session_id_header.as_deref(),
            Some(MCP_KEY_MIGRATION_UPSTREAM_SESSION),
            "the replay presents the migrated upstream session id"
        );
        // The client's next call on the SAME proxy session id is served by the
        // migrated binding, so the migration is durable and not a one-shot
        // patch of a single request.
        let next_follow_up = &outcome.calls[4];
        assert_eq!(next_follow_up.method, "tools/list");
        assert_eq!(next_follow_up.tavily_api_key, outcome.migrated_key);
        assert_eq!(
            next_follow_up.upstream_session_id_header.as_deref(),
            Some(MCP_KEY_MIGRATION_UPSTREAM_SESSION)
        );

        let pool = connect_sqlite_test_pool(&outcome.db_str).await;
        let pinned_key_id = fetch_mcp_migration_key_id(&pool, &outcome.pinned_key).await;
        let migrated_key_id = fetch_mcp_migration_key_id(&pool, &outcome.migrated_key).await;
        assert_ne!(pinned_key_id, migrated_key_id);
        let (upstream_key_id, upstream_session_id, revoked_at) =
            fetch_mcp_session_upstream_binding(&pool, &outcome.proxy_session_id).await;
        assert_eq!(
            upstream_key_id.as_deref(),
            Some(migrated_key_id.as_str()),
            "the session row must name the key it now rides"
        );
        assert_eq!(
            upstream_session_id.as_deref(),
            Some(MCP_KEY_MIGRATION_UPSTREAM_SESSION)
        );
        assert_eq!(revoked_at, None, "a migrated session is never revoked");
    }

    #[tokio::test]
    async fn mcp_follow_up_migrates_pinned_session_off_auth_blocked_key() {
        let outcome = run_mcp_key_migration_scenario(
            "mcp-migrate-auth-blocked",
            McpKeyMigrationRejection::AuthBlocked403,
            false,
        )
        .await;
        assert_mcp_session_migrated(&outcome).await;
        let _ = std::fs::remove_file(&outcome.db_path);
    }

    #[tokio::test]
    async fn mcp_follow_up_migrates_pinned_session_off_exhausted_key() {
        let outcome = run_mcp_key_migration_scenario(
            "mcp-migrate-exhausted",
            McpKeyMigrationRejection::QuotaExhausted432,
            false,
        )
        .await;
        assert_mcp_session_migrated(&outcome).await;
        let _ = std::fs::remove_file(&outcome.db_path);
    }

    #[tokio::test]
    async fn mcp_follow_up_serves_original_rejection_when_reinitialize_is_refused() {
        let outcome = run_mcp_key_migration_scenario(
            "mcp-migrate-reinit-refused",
            McpKeyMigrationRejection::AuthBlocked403,
            true,
        )
        .await;

        // The ORIGINAL rejection, byte-for-byte: same status, same body the
        // dead key's attempt produced.
        assert_eq!(outcome.follow_up_status, StatusCode::FORBIDDEN);
        assert_eq!(
            outcome.follow_up_body,
            Bytes::from(MCP_KEY_MIGRATION_REJECTION_BODY),
            "a refused re-initialize must serve the original rejection bytes"
        );
        assert_eq!(outcome.follow_up_tools, None);

        // Bounded: the failing follow-up cost exactly TWO upstream calls (its
        // own rejection + the refused re-initialize). No replay, no loop.
        assert_eq!(
            outcome.calls.len(),
            3,
            "unexpected upstream call chain: {:?}",
            outcome.calls
        );
        assert_eq!(
            outcome.calls_for("tools/list").len(),
            1,
            "the original call must never be replayed after a refused re-initialize"
        );
        let refused = &outcome.calls[2];
        assert_eq!(refused.method, "initialize");
        assert_eq!(refused.tavily_api_key, outcome.migrated_key);

        // No rebind happened: the row still names the dead key and the original
        // upstream session, and it is still live (the client may retry).
        let pool = connect_sqlite_test_pool(&outcome.db_str).await;
        let pinned_key_id = fetch_mcp_migration_key_id(&pool, &outcome.pinned_key).await;
        let (upstream_key_id, upstream_session_id, revoked_at) =
            fetch_mcp_session_upstream_binding(&pool, &outcome.proxy_session_id).await;
        assert_eq!(upstream_key_id.as_deref(), Some(pinned_key_id.as_str()));
        assert_eq!(
            upstream_session_id.as_deref(),
            Some(MCP_KEY_MIGRATION_PINNED_SESSION)
        );
        assert_eq!(revoked_at, None);

        let _ = std::fs::remove_file(&outcome.db_path);
    }
