// Pinned-session key migration (plan provider-key-429-retry todo 13, battery todo 14).
    //
    // `include!`d from `mcp_rebalance_and_follow_up.rs` because that file sits at
    // its `rust_source_line_budgets` ceiling (3041/3050) and the plan forbids
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
    //
    // Todo 14 adds the invariant battery around that seam, one case per property
    // a reviewer has to be able to point at:
    //   * in-place rebind - one row, same `created_at`, same owner and gateway
    //     mode; the new UPSTREAM session id never reaches a client header;
    //   * revocation scope - a key-scoped sweep of the dead key leaves the
    //     migrated session alive and serving, the same sweep on the key it now
    //     rides kills it and the client is told to reconnect;
    //   * the second abort branch - no eligible key at all serves the original
    //     rejection without even attempting a re-`initialize`;
    //   * the signal split from the 429 side - a rate-limited session waits and
    //     retries its OWN key once and never migrates, spare key or not;
    //   * billing - a migrated request settles exactly ONCE while every upstream
    //     attempt stays visible in `request_logs`;
    //   * HA shape - a session imported from a peer row that still names a dead
    //     key migrates once, then rides the local row: exactly one extra
    //     `initialize` per request, no second peer lookup, never a loop.

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

    /// Live count of every upstream call the migration mock has seen. Cases that
    /// keep talking to the session after their first assertion read it again to
    /// prove a rejected or migrated call never reached upstream a second time.
    fn snapshot_mcp_migration_calls(
        call_log: &Arc<Mutex<Vec<KeyMigrationSessionCall>>>,
    ) -> Vec<KeyMigrationSessionCall> {
        call_log
            .lock()
            .expect("key migration calls lock poisoned")
            .clone()
    }

    /// One follow-up on an existing proxy session id, sent exactly the way the
    /// shared driver sends it. `charging` picks the billable `tools/call` shape
    /// over a plain `tools/list`.
    async fn send_mcp_key_migration_follow_up(
        client: &Client,
        url: &str,
        proxy_session_id: &str,
        id: &str,
        charging: bool,
    ) -> (StatusCode, Bytes, Option<String>) {
        let body = if charging {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "tavily-search",
                    "arguments": {
                        "query": "mcp key migration billing",
                        "search_depth": "basic"
                    }
                }
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/list"
            })
        };
        let response = client
            .post(url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", "2025-03-26")
            .header("mcp-session-id", proxy_session_id)
            .json(&body)
            .send()
            .await
            .expect("follow-up request");
        let status = response.status();
        let session_header = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.bytes().await.expect("follow-up body");
        (status, bytes, session_header)
    }

    /// Tools count of a follow-up body, when it was a successful `tools/list`.
    fn mcp_follow_up_tool_count(body: &Bytes) -> Option<usize> {
        serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|body| body["result"]["tools"].as_array().map(Vec::len))
    }

    /// Knobs the shared driver takes. [`McpKeyMigrationSetup::new`] reproduces
    /// the todo-13 defaults exactly: a two-key pool, a `tools/list` follow-up, a
    /// token with no OAuth user, and a second follow-up on the same proxy session
    /// id unless the re-`initialize` is refused.
    #[derive(Debug)]
    struct McpKeyMigrationSetup {
        /// Pooled keys the proxy is created with. The mock pins whichever one the
        /// client's `initialize` happens to ride, so a case never depends on pool
        /// ordering; ONE key is the "no eligible key" shape.
        pool_size: usize,
        rejection: McpKeyMigrationRejection,
        reject_reinitialize: bool,
        /// Send the client follow-up as a charging `tools/call` instead of a
        /// `tools/list`, so the settlement rows are worth counting.
        charging_follow_up: bool,
        /// Bind the access token to an OAuth user: the production key-scoped
        /// session sweep (`revoke_mcp_sessions_for_user_key`) needs a user
        /// subject on the row to match.
        bind_user: bool,
        /// Skip the second follow-up, for cases whose client is answered with a
        /// rejection or whose billing math counts one request only.
        skip_second_follow_up: bool,
    }

    impl McpKeyMigrationSetup {
        fn new(rejection: McpKeyMigrationRejection, reject_reinitialize: bool) -> Self {
            Self {
                pool_size: 2,
                rejection,
                reject_reinitialize,
                charging_follow_up: false,
                bind_user: false,
                skip_second_follow_up: false,
            }
        }

        fn pool_keys(&self, prefix: &str) -> Vec<String> {
            const SUFFIXES: [&str; 4] = ["a", "b", "c", "d"];
            assert!(
                self.pool_size >= 1 && self.pool_size <= SUFFIXES.len(),
                "a migration case pools between one and four keys"
            );
            SUFFIXES
                .into_iter()
                .take(self.pool_size)
                .map(|suffix| format!("tvly-{prefix}-{suffix}"))
                .collect()
        }

        fn send_second_follow_up(&self) -> bool {
            !self.reject_reinitialize && !self.skip_second_follow_up
        }
    }

    /// Everything one migration case asserts on, produced by the shared driver.
    struct McpKeyMigrationOutcome {
        proxy_session_id: String,
        /// Status of the follow-up the client sent on the pinned (dead) key.
        follow_up_status: StatusCode,
        /// Raw bytes of that follow-up response - the byte-for-byte passthrough
        /// evidence when the migration aborts.
        follow_up_body: Bytes,
        /// `mcp-session-id` header of that response: the client-visible session
        /// id, which must stay the PROXY id across a migration.
        follow_up_session_header: Option<String>,
        /// Tool count of that response, when it was a successful `tools/list`.
        follow_up_tools: Option<usize>,
        /// Second follow-up on the SAME proxy session id, sent only when the
        /// migration was expected to succeed: proves the migrated binding is
        /// usable and the proxy session id is stable.
        second_follow_up_status: Option<StatusCode>,
        second_follow_up_tools: Option<usize>,
        second_follow_up_session_header: Option<String>,
        /// Every upstream call in order, as of the moment the driver returned.
        calls: Vec<KeyMigrationSessionCall>,
        /// The mock's live call log, for cases that keep talking to the session.
        call_log: Arc<Mutex<Vec<KeyMigrationSessionCall>>>,
        /// Pooled key the session was pinned to (the one the mock rejects).
        pinned_key: String,
        /// The key the migration moved to: the one the re-`initialize` rode when
        /// there was one, otherwise the first other pooled key.
        migrated_key: String,
        /// Client URL, token and (when the case binds one) OAuth user, so a case
        /// can keep driving the same session and trigger production key sweeps.
        url: String,
        token_id: String,
        user_id: Option<String>,
        proxy: TavilyProxy,
        /// Row and log state captured immediately BEFORE the follow-up, so a case
        /// can prove the migration rewrote one row in place and settled once.
        token_logs_before_follow_up: i64,
        request_logs_before_follow_up: i64,
        created_at_before_follow_up: i64,
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
        run_mcp_key_migration_scenario_with(
            prefix,
            &McpKeyMigrationSetup::new(rejection, reject_reinitialize),
        )
        .await
    }

    /// The same driver with every knob exposed. The todo-13 cases reach it
    /// through [`run_mcp_key_migration_scenario`] with the defaults.
    async fn run_mcp_key_migration_scenario_with(
        prefix: &str,
        setup: &McpKeyMigrationSetup,
    ) -> McpKeyMigrationOutcome {
        let db_path = temp_db_path(prefix);
        let db_str = db_path.to_string_lossy().to_string();
        let pool_keys = setup.pool_keys(prefix);
        let (upstream_addr, call_log) = spawn_mock_mcp_upstream_for_session_key_migration(
            pool_keys.clone(),
            setup.rejection,
            setup.reject_reinitialize,
            None,
        )
        .await;
        let upstream = format!("http://{}", upstream_addr);

        let proxy = TavilyProxy::with_endpoint(pool_keys.clone(), &upstream, &db_str)
            .await
            .expect("proxy created");
        let user_id = if setup.bind_user {
            let user = proxy
                .upsert_oauth_account(&OAuthAccountProfile {
                    provider: "linuxdo".to_string(),
                    provider_user_id: format!("{prefix}-user"),
                    username: Some(format!("{prefix}-user")),
                    name: Some("Key Migration User".to_string()),
                    avatar_template: None,
                    active: true,
                    trust_level: Some(2),
                    raw_payload_json: None,
                })
                .await
                .expect("upsert oauth user");
            Some(user.user_id)
        } else {
            None
        };
        let access_token = match user_id.as_deref() {
            Some(user_id) => {
                proxy
                    .ensure_user_token_binding(user_id, Some(&format!("linuxdo:{prefix}")))
                    .await
                    .expect("bind user token")
            }
            None => {
                proxy
                    .create_access_token(Some(prefix))
                    .await
                    .expect("create access token")
            }
        };
        let token_id = access_token.id.clone();
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

        // What the follow-up is about to touch: the settlement rows, the attempt
        // rows and the identity of the row the session already owns.
        let snapshot_pool = connect_sqlite_test_pool(&db_str).await;
        let token_logs_before_follow_up: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM auth_token_logs")
                .fetch_one(&snapshot_pool)
                .await
                .expect("count auth_token_logs before the follow-up");
        let request_logs_before_follow_up: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM request_logs")
                .fetch_one(&snapshot_pool)
                .await
                .expect("count request_logs before the follow-up");
        let created_at_before_follow_up: i64 = sqlx::query_scalar(
            "SELECT created_at FROM mcp_sessions WHERE proxy_session_id = ? LIMIT 1",
        )
        .bind(&proxy_session_id)
        .fetch_one(&snapshot_pool)
        .await
        .expect("fetch the session row created_at before the follow-up");

        let (follow_up_status, follow_up_body, follow_up_session_header) =
            send_mcp_key_migration_follow_up(
                &client,
                &url,
                &proxy_session_id,
                "tools-migration",
                setup.charging_follow_up,
            )
            .await;
        let follow_up_tools = mcp_follow_up_tool_count(&follow_up_body);

        let mut second_follow_up_status = None;
        let mut second_follow_up_tools = None;
        let mut second_follow_up_session_header = None;
        if setup.send_second_follow_up() {
            let (status, body, session_header) = send_mcp_key_migration_follow_up(
                &client,
                &url,
                &proxy_session_id,
                "tools-after-migration",
                setup.charging_follow_up,
            )
            .await;
            second_follow_up_status = Some(status);
            second_follow_up_tools = mcp_follow_up_tool_count(&body);
            second_follow_up_session_header = session_header;
        }

        let recorded = snapshot_mcp_migration_calls(&call_log);
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
        let migrated_key = recorded
            .iter()
            .find(|call| call.method == "initialize" && call.tavily_api_key != pinned_key)
            .map(|call| call.tavily_api_key.clone())
            .or_else(|| {
                pool_keys
                    .iter()
                    .find(|key| key.as_str() != pinned_key)
                    .cloned()
            })
            .unwrap_or_else(|| pinned_key.clone());

        McpKeyMigrationOutcome {
            proxy_session_id,
            follow_up_status,
            follow_up_body,
            follow_up_session_header,
            follow_up_tools,
            second_follow_up_status,
            second_follow_up_tools,
            second_follow_up_session_header,
            calls: recorded,
            call_log,
            pinned_key,
            migrated_key,
            url,
            token_id,
            user_id,
            proxy,
            token_logs_before_follow_up,
            request_logs_before_follow_up,
            created_at_before_follow_up,
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

    /// Todo 14 case 1, strengthened: the migration is an in-place rebind of the
    /// ONE row the session already owned - same `created_at`, same owning token,
    /// still upstream gateway mode - and it leaks nothing client-side: no
    /// response carries the new upstream session id, so the proxy id handed out
    /// at `initialize` stays the client's only handle.
    #[tokio::test]
    async fn mcp_follow_up_key_migration_rebinds_one_row_in_place_and_hides_the_upstream_id() {
        let outcome = run_mcp_key_migration_scenario(
            "mcp-migrate-in-place",
            McpKeyMigrationRejection::AuthBlocked403,
            false,
        )
        .await;
        assert_mcp_session_migrated(&outcome).await;

        // Seamless from the client side, and leak-free: the handler only
        // rewrites a response `mcp-session-id` when the upstream answer carries
        // one, and these follow-up answers do not - so a migrated response
        // exposes NO upstream session id at all. The client's only handle stays
        // the proxy id it was issued at `initialize`, which both follow-ups above
        // kept using successfully.
        assert_eq!(
            outcome.follow_up_session_header, None,
            "a migrated response must not expose the new upstream session id"
        );
        assert_eq!(
            outcome.second_follow_up_session_header, None,
            "the next response must not expose an upstream session id either"
        );

        let pool = connect_sqlite_test_pool(&outcome.db_str).await;
        let other_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM mcp_sessions WHERE proxy_session_id <> ?")
                .bind(&outcome.proxy_session_id)
                .fetch_one(&pool)
                .await
                .expect("count session rows the migration might have added");
        assert_eq!(
            other_rows, 0,
            "a migration rebinds the row it has; it never opens a second session"
        );

        let row = sqlx::query(
            r#"
            SELECT auth_token_id, gateway_mode, created_at, revoked_at, revoke_reason
            FROM mcp_sessions
            WHERE proxy_session_id = ?
            LIMIT 1
            "#,
        )
        .bind(&outcome.proxy_session_id)
        .fetch_one(&pool)
        .await
        .expect("fetch the migrated session row");
        assert_eq!(
            row.try_get::<i64, _>("created_at").unwrap(),
            outcome.created_at_before_follow_up,
            "the rebind UPDATEs the existing row instead of recreating it"
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("auth_token_id")
                .unwrap()
                .as_deref(),
            Some(outcome.token_id.as_str()),
            "the migrated session still belongs to the same token"
        );
        assert_eq!(
            row.try_get::<String, _>("gateway_mode").unwrap(),
            tavily_hikari::MCP_GATEWAY_MODE_UPSTREAM,
            "a migration never switches the session into rebalance mode"
        );
        assert_eq!(row.try_get::<Option<i64>, _>("revoked_at").unwrap(), None);
        assert_eq!(
            row.try_get::<Option<String>, _>("revoke_reason").unwrap(),
            None
        );

        let _ = std::fs::remove_file(&outcome.db_path);
    }

    /// Todo 14 case 2: revocation follows the key the row names NOW. Sweeping the
    /// dead key - through the production path that fires
    /// `revoke_mcp_sessions_for_user_key`, a primary-affinity rebind - leaves the
    /// migrated session alive and serving; the same sweep on the key it migrated
    /// to kills it, and the client is told to reconnect instead of being served
    /// on a dead binding.
    #[tokio::test]
    async fn mcp_follow_up_key_migration_survives_a_dead_key_sweep_and_dies_with_its_new_key() {
        let setup = McpKeyMigrationSetup {
            // A third key so both rebinds below have somewhere to land.
            pool_size: 3,
            bind_user: true,
            ..McpKeyMigrationSetup::new(McpKeyMigrationRejection::AuthBlocked403, false)
        };
        let outcome =
            run_mcp_key_migration_scenario_with("mcp-migrate-revoke-scope", &setup).await;
        assert_mcp_session_migrated(&outcome).await;

        let pool = connect_sqlite_test_pool(&outcome.db_str).await;
        let user_id = outcome
            .user_id
            .clone()
            .expect("this case binds the token to an oauth user");
        let dead_key_id = fetch_mcp_migration_key_id(&pool, &outcome.pinned_key).await;
        let migrated_key_id = fetch_mcp_migration_key_id(&pool, &outcome.migrated_key).await;
        let calls_after_migration = snapshot_mcp_migration_calls(&outcome.call_log).len();

        // (a) Sweep the DEAD key: disable it, point the user's primary affinity
        // at it and let the production acquire rebind - the rebind is what
        // revokes every session still riding that key.
        outcome
            .proxy
            .disable_key_by_id(&dead_key_id)
            .await
            .expect("disable the dead key");
        outcome
            .proxy
            .seed_user_primary_api_key_affinity_for_test(&user_id, &dead_key_id)
            .await
            .expect("seed the primary affinity to the dead key");
        let rebound = outcome
            .proxy
            .acquire_key_id_for_test(Some(outcome.token_id.as_str()))
            .await
            .expect("rebind the primary affinity off the dead key");
        assert_ne!(
            rebound, dead_key_id,
            "the affinity rebind must move off the disabled key"
        );

        let (key_after_dead_sweep, _, revoked_after_dead_sweep) =
            fetch_mcp_session_upstream_binding(&pool, &outcome.proxy_session_id).await;
        assert_eq!(
            key_after_dead_sweep.as_deref(),
            Some(migrated_key_id.as_str()),
            "a dead-key sweep must not touch the migrated binding"
        );
        assert_eq!(
            revoked_after_dead_sweep, None,
            "revoking the key the session LEFT must not kill the migrated session"
        );

        let (status_after_dead_sweep, _, _) = send_mcp_key_migration_follow_up(
            &Client::new(),
            &outcome.url,
            &outcome.proxy_session_id,
            "tools-after-dead-key-sweep",
            false,
        )
        .await;
        assert_eq!(
            status_after_dead_sweep,
            StatusCode::OK,
            "the migrated session keeps serving after the dead key is revoked"
        );
        let calls_after_dead_sweep = snapshot_mcp_migration_calls(&outcome.call_log);
        assert_eq!(
            calls_after_dead_sweep.len(),
            calls_after_migration + 1,
            "serving after a dead-key sweep costs ONE upstream call: no re-initialize, \
             no second migration, got {:?}",
            calls_after_dead_sweep
        );
        assert_eq!(
            calls_after_dead_sweep.last().map(|call| call.method.as_str()),
            Some("tools/list")
        );
        assert_eq!(
            calls_after_dead_sweep
                .last()
                .map(|call| call.tavily_api_key.as_str()),
            Some(outcome.migrated_key.as_str())
        );

        // (b) Sweep the key the session RIDES NOW: the same production rebind now
        // matches the row, so the session dies and the client must reconnect.
        outcome
            .proxy
            .disable_key_by_id(&migrated_key_id)
            .await
            .expect("disable the migrated key");
        outcome
            .proxy
            .seed_user_primary_api_key_affinity_for_test(&user_id, &migrated_key_id)
            .await
            .expect("seed the primary affinity to the migrated key");
        let rebound_again = outcome
            .proxy
            .acquire_key_id_for_test(Some(outcome.token_id.as_str()))
            .await
            .expect("rebind the primary affinity off the migrated key");
        assert_ne!(
            rebound_again, migrated_key_id,
            "the affinity rebind must move off the disabled migrated key"
        );

        let revoke_reason: Option<String> = sqlx::query_scalar(
            r#"
            SELECT revoke_reason
            FROM mcp_sessions
            WHERE proxy_session_id = ?
            LIMIT 1
            "#,
        )
        .bind(&outcome.proxy_session_id)
        .fetch_one(&pool)
        .await
        .expect("fetch the revoked session row");
        let (key_after_new_sweep, _, revoked_after_new_sweep) =
            fetch_mcp_session_upstream_binding(&pool, &outcome.proxy_session_id).await;
        assert_eq!(
            key_after_new_sweep.as_deref(),
            Some(migrated_key_id.as_str()),
            "the revoked row still names the key it rode"
        );
        assert!(
            revoked_after_new_sweep.is_some(),
            "revoking the key a migrated session rides must kill that session"
        );
        assert_eq!(
            revoke_reason.as_deref(),
            Some("primary_api_key_rebound"),
            "the sweep that killed it is the production key-scoped revocation"
        );

        let (status_after_new_sweep, body_after_new_sweep, _) =
            send_mcp_key_migration_follow_up(
                &Client::new(),
                &outcome.url,
                &outcome.proxy_session_id,
                "tools-after-new-key-sweep",
                false,
            )
            .await;
        assert_eq!(
            status_after_new_sweep,
            StatusCode::NOT_FOUND,
            "a revoked session is answered locally, never served on a dead binding"
        );
        let revoked_body: Value =
            serde_json::from_slice(&body_after_new_sweep).expect("parse the revoked-session body");
        assert_eq!(revoked_body["error"].as_str(), Some("session_unavailable"));
        assert_eq!(
            snapshot_mcp_migration_calls(&outcome.call_log).len(),
            calls_after_dead_sweep.len(),
            "a revoked session never reaches upstream again"
        );

        let _ = std::fs::remove_file(&outcome.db_path);
    }

    /// Todo 14 case 3, strengthened: the OTHER abort branch. With no second
    /// eligible key the ladder never even attempts a re-`initialize` - the client
    /// gets the original rejection bytes and the whole session cost TWO upstream
    /// calls.
    #[tokio::test]
    async fn mcp_follow_up_serves_original_rejection_when_no_second_key_is_eligible() {
        let setup = McpKeyMigrationSetup {
            pool_size: 1,
            skip_second_follow_up: true,
            ..McpKeyMigrationSetup::new(McpKeyMigrationRejection::AuthBlocked403, false)
        };
        let outcome =
            run_mcp_key_migration_scenario_with("mcp-migrate-no-second-key", &setup).await;

        assert_eq!(outcome.follow_up_status, StatusCode::FORBIDDEN);
        assert_eq!(
            outcome.follow_up_body,
            Bytes::from(MCP_KEY_MIGRATION_REJECTION_BODY),
            "no eligible key must serve the original rejection bytes"
        );
        assert_eq!(outcome.follow_up_tools, None);
        assert_eq!(outcome.second_follow_up_status, None);

        assert_eq!(
            outcome.calls.len(),
            2,
            "the pinning initialize plus the rejected call, nothing else: {:?}",
            outcome.calls
        );
        assert_eq!(
            outcome.calls_for("initialize").len(),
            1,
            "with no eligible key there is no re-initialize to attempt"
        );
        assert_eq!(outcome.calls_for("tools/list").len(), 1);
        assert!(
            outcome
                .calls
                .iter()
                .all(|call| call.tavily_api_key == outcome.pinned_key),
            "the single pooled key is the only key that may reach upstream"
        );

        let pool = connect_sqlite_test_pool(&outcome.db_str).await;
        let pinned_key_id = fetch_mcp_migration_key_id(&pool, &outcome.pinned_key).await;
        let (upstream_key_id, upstream_session_id, revoked_at) =
            fetch_mcp_session_upstream_binding(&pool, &outcome.proxy_session_id).await;
        assert_eq!(
            upstream_key_id.as_deref(),
            Some(pinned_key_id.as_str()),
            "the row keeps its binding when there is nothing to migrate to"
        );
        assert_eq!(
            upstream_session_id.as_deref(),
            Some(MCP_KEY_MIGRATION_PINNED_SESSION)
        );
        assert_eq!(
            revoked_at, None,
            "an unmigratable rejection is not a reason to revoke the session"
        );

        let _ = std::fs::remove_file(&outcome.db_path);
    }

    /// Todo 14 case 4: the signal split seen from the 429 side. A rate-limited
    /// pinned session waits and retries its OWN key once - it never migrates,
    /// even though a healthy spare key sat in the pool the whole time. This is
    /// the guard that keeps the todo-13 migration off the wait semantics; the
    /// wait-and-retry behavior itself stays pinned by
    /// `mcp_follow_up_retries_once_after_retry_after_and_keeps_session_pinned`.
    #[tokio::test]
    async fn mcp_follow_up_rate_limited_session_retries_its_own_key_and_never_migrates() {
        let setup = McpKeyMigrationSetup {
            skip_second_follow_up: true,
            ..McpKeyMigrationSetup::new(McpKeyMigrationRejection::RateLimited429, false)
        };
        let outcome = run_mcp_key_migration_scenario_with("mcp-migrate-429-stays", &setup).await;

        assert_eq!(outcome.follow_up_status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            outcome.follow_up_body,
            Bytes::from(MCP_KEY_MIGRATION_RATE_LIMIT_BODY),
            "a rate-limited session is served the 429 body, not a migration"
        );
        assert_eq!(outcome.follow_up_tools, None);

        // initialize + the rejected call + ONE same-key retry.
        assert_eq!(
            outcome.calls.len(),
            3,
            "unexpected upstream call chain: {:?}",
            outcome.calls
        );
        assert_eq!(
            outcome.calls_for("initialize").len(),
            1,
            "a 429 must never trigger a re-initialize"
        );
        let retries = outcome.calls_for("tools/list");
        assert_eq!(retries.len(), 2, "wait plus exactly one same-key retry");
        assert!(
            retries
                .iter()
                .all(|call| call.tavily_api_key == outcome.pinned_key),
            "both attempts ride the pinned key: {:?}",
            retries
        );
        assert!(
            retries.iter().all(|call| call
                .upstream_session_id_header
                .as_deref()
                == Some(MCP_KEY_MIGRATION_PINNED_SESSION)),
            "both attempts ride the pinned upstream session: {:?}",
            retries
        );
        assert!(
            !outcome
                .calls
                .iter()
                .any(|call| call.tavily_api_key == outcome.migrated_key),
            "the spare pooled key must never be touched by a 429"
        );

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
        let (_, last_rate_limited_at, last_rate_limit_reason): (Option<i64>, Option<i64>, Option<String>) =
            sqlx::query_as(
                r#"
                SELECT rate_limited_until, last_rate_limited_at, last_rate_limit_reason
                FROM mcp_sessions
                WHERE proxy_session_id = ?
                LIMIT 1
                "#,
            )
            .bind(&outcome.proxy_session_id)
            .fetch_one(&pool)
            .await
            .expect("fetch the rate-limit markers of the pinned session");
        assert!(
            last_rate_limited_at.is_some(),
            "the 429 keeps its own rate-limit marker instead of a migration"
        );
        assert_eq!(
            last_rate_limit_reason.as_deref(),
            Some("upstream_rate_limited_429")
        );

        let _ = std::fs::remove_file(&outcome.db_path);
    }

    /// Todo 14 case 5: a migrated request settles billing exactly ONCE. The
    /// ladder costs three upstream attempts - the rejected call, the
    /// proxy-driven re-`initialize` and the replay - and every one of them stays
    /// visible in `request_logs`, but the client is charged a single credit on a
    /// single `auth_token_logs` row.
    #[tokio::test]
    async fn mcp_follow_up_key_migration_settles_billing_exactly_once() {
        let setup = McpKeyMigrationSetup {
            charging_follow_up: true,
            skip_second_follow_up: true,
            ..McpKeyMigrationSetup::new(McpKeyMigrationRejection::AuthBlocked403, false)
        };
        let outcome = run_mcp_key_migration_scenario_with("mcp-migrate-billing", &setup).await;

        assert_eq!(
            outcome.follow_up_status,
            StatusCode::OK,
            "the client only ever sees the migrated success"
        );
        assert_eq!(
            outcome.calls.len(),
            4,
            "pin, reject, re-initialize, replay - and nothing more: {:?}",
            outcome.calls
        );
        assert_eq!(outcome.calls_for("initialize").len(), 2);
        assert_eq!(outcome.calls_for("tools/call").len(), 2);

        let pool = connect_sqlite_test_pool(&outcome.db_str).await;
        let token_logs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_token_logs")
            .fetch_one(&pool)
            .await
            .expect("count auth_token_logs after the migrated request");
        assert_eq!(
            token_logs,
            outcome.token_logs_before_follow_up + 1,
            "a migrated request settles exactly once"
        );
        let row = sqlx::query(
            r#"
            SELECT token_id, billing_state, business_credits, proxy_session_id
            FROM auth_token_logs
            ORDER BY id DESC
            LIMIT 1
            "#,
        )
        .fetch_one(&pool)
        .await
        .expect("fetch the single settlement row");
        assert_eq!(
            row.try_get::<String, _>("token_id").unwrap(),
            outcome.token_id,
            "the one settlement is charged to the calling token"
        );
        assert_eq!(row.try_get::<String, _>("billing_state").unwrap(), "charged");
        assert_eq!(
            row.try_get::<Option<i64>, _>("business_credits").unwrap(),
            Some(1),
            "one migrated search charges one credit, not one per upstream attempt"
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("proxy_session_id")
                .unwrap()
                .as_deref(),
            Some(outcome.proxy_session_id.as_str()),
            "the settlement stays attributed to the stable proxy session"
        );

        let request_logs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs")
            .fetch_one(&pool)
            .await
            .expect("count request_logs after the migrated request");
        assert_eq!(
            request_logs,
            outcome.request_logs_before_follow_up + 3,
            "every upstream attempt of the migrated request stays visible"
        );

        let _ = std::fs::remove_file(&outcome.db_path);
    }

    /// Internal token the todo-14 HA cases configure on both sides of the peer
    /// lookup.
    const MCP_KEY_MIGRATION_HA_INTERNAL_TOKEN: &str = "mcp-migration-ha-internal-token";

    /// One dual-active node, its mock upstream, and the single mock peer that owns
    /// a pinned session whose upstream key is dead from the first call.
    struct McpHaPeerMigrationHarness {
        /// Client URL of the local node's `/mcp`, caller token included.
        url: String,
        /// Proxy session id the peer serves its row under.
        proxy_session_id: String,
        call_log: Arc<Mutex<Vec<KeyMigrationSessionCall>>>,
        /// Counts every internal mcp-session lookup the local node sends.
        peer_lookups: Arc<AtomicUsize>,
        /// Pooled secret the peer row names; the row itself carries its key id.
        dead_api_key: String,
        /// The migration target, and its `api_keys.id`.
        spare_api_key: String,
        spare_key_id: String,
        db_str: String,
        db_path: PathBuf,
    }

    /// Shared HA setup for the two peer-imported cases: this node is seeded as the
    /// dual-active leader, so it serves business traffic and may keep its own copy
    /// of an imported row, and the mock peer answers the internal lookup with a
    /// binding whose `upstream_key_id` the mock upstream rejects. Nothing is
    /// written to the local store here, so the first follow-up can only be
    /// resolved through the peer.
    async fn spawn_mcp_ha_peer_migration_harness(prefix: &str) -> McpHaPeerMigrationHarness {
        let db_path = temp_db_path(prefix);
        let db_str = db_path.to_string_lossy().to_string();
        let dead_api_key = format!("tvly-{prefix}-a");
        let spare_api_key = format!("tvly-{prefix}-b");
        // `pre_pinned_key` makes the mock reject the key the PEER row names from
        // the first call: this session never sends an `initialize` of its own.
        let (upstream_addr, call_log) = spawn_mock_mcp_upstream_for_session_key_migration(
            vec![dead_api_key.clone(), spare_api_key.clone()],
            McpKeyMigrationRejection::AuthBlocked403,
            false,
            Some(dead_api_key.clone()),
        )
        .await;
        let upstream = format!("http://{}", upstream_addr);

        let proxy = TavilyProxy::with_endpoint(
            vec![dead_api_key.clone(), spare_api_key.clone()],
            &upstream,
            &db_str,
        )
        .await
        .expect("proxy created");
        let access_token = proxy
            .create_access_token(Some(prefix))
            .await
            .expect("create access token");

        let pool = connect_sqlite_test_pool(&db_str).await;
        let dead_key_id = fetch_mcp_migration_key_id(&pool, &dead_api_key).await;
        let spare_key_id = fetch_mcp_migration_key_id(&pool, &spare_api_key).await;
        assert_ne!(dead_key_id, spare_key_id);

        // The peer's row: same shape `get_internal_ha_mcp_session` serves, bound
        // to the dead key and to this caller's token.
        let proxy_session_id = format!("peer-{prefix}-session");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the unix epoch")
            .as_secs() as i64;
        let peer_binding = tavily_hikari::McpSessionBinding {
            proxy_session_id: proxy_session_id.clone(),
            upstream_session_id: Some(MCP_KEY_MIGRATION_PINNED_SESSION.to_string()),
            upstream_key_id: Some(dead_key_id.clone()),
            auth_token_id: Some(access_token.id.clone()),
            user_id: None,
            protocol_version: Some("2025-03-26".to_string()),
            last_event_id: None,
            gateway_mode: tavily_hikari::MCP_GATEWAY_MODE_UPSTREAM.to_string(),
            experiment_variant: tavily_hikari::MCP_EXPERIMENT_VARIANT_CONTROL.to_string(),
            ab_bucket: None,
            routing_subject_hash: None,
            fallback_reason: None,
            rate_limited_until: None,
            last_rate_limited_at: None,
            last_rate_limit_reason: None,
            created_at: now,
            updated_at: now,
            expires_at: now + 3600,
            revoked_at: None,
            revoke_reason: None,
        };
        let (peer_addr, peer_lookups) = spawn_mock_ha_peer_mcp_session_server(
            peer_binding,
            MCP_KEY_MIGRATION_HA_INTERNAL_TOKEN.to_string(),
        )
        .await;

        let ha = tavily_hikari::HaRuntime::new(tavily_hikari::HaConfig {
            mode: tavily_hikari::HaMode::ActiveStandby,
            node_id: "node-local".to_string(),
            database_path: Some(db_str.clone()),
            source_kind: Some(tavily_hikari::HaSourceKind::OriginGroup),
            source_origin_group_id: Some("og-core".to_string()),
            core_dual_active: true,
            internal_token: Some(MCP_KEY_MIGRATION_HA_INTERNAL_TOKEN.to_string()),
            peer_nodes: vec![tavily_hikari::HaPeerNodeConfig {
                node_id: "node-peer".to_string(),
                admin_base_url: format!("http://{peer_addr}"),
                public_origin: "node-peer-public:443".to_string(),
                role_hint: tavily_hikari::HaPeerRoleHint::StandbyCandidate,
            }],
            ..tavily_hikari::HaConfig::default()
        });
        ha.apply_dual_active_leader(Some("node-local".to_string()))
            .await
            .expect("seed this node as the dual-active leader");
        let proxy_addr =
            spawn_proxy_server_with_dev_and_ha(proxy.clone(), upstream.clone(), false, ha).await;
        let url = format!(
            "http://{}/mcp?tavilyApiKey={}",
            proxy_addr, access_token.token
        );

        McpHaPeerMigrationHarness {
            url,
            proxy_session_id,
            call_log,
            peer_lookups,
            dead_api_key,
            spare_api_key,
            spare_key_id,
            db_str,
            db_path,
        }
    }

    /// Todo 14 case 6, the HA shape: a pinned session whose row lives on a PEER
    /// and still names a dead key. The local node imports the binding, migrates
    /// it once and then rides its own local row - so the peer lookup happens
    /// once, the extra `initialize` happens once per request, the client is
    /// always answered, and a peer row that keeps the dead key can never turn
    /// into a migration loop. The shape where the local copy does NOT survive is
    /// pinned separately by
    /// `mcp_follow_up_peer_row_that_keeps_the_dead_key_remigrates_per_request`.
    #[tokio::test]
    async fn mcp_follow_up_peer_imported_session_migrates_once_and_then_rides_the_local_row() {
        let harness = spawn_mcp_ha_peer_migration_harness("mcp-migrate-ha-peer").await;
        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let client = Client::new();

        // The local store has NO row for this proxy session id yet: the follow-up
        // is served from the peer import.
        let local_rows_before: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM mcp_sessions WHERE proxy_session_id = ?")
                .bind(&harness.proxy_session_id)
                .fetch_one(&pool)
                .await
                .expect("count local session rows before the follow-up");
        assert_eq!(local_rows_before, 0, "the session only exists on the peer");

        let (first_status, first_body, first_session_header) =
            send_mcp_key_migration_follow_up(
                &client,
                &harness.url,
                &harness.proxy_session_id,
                "tools-peer-migration",
                false,
            )
            .await;
        assert_eq!(
            first_status,
            StatusCode::OK,
            "a peer-imported session on a dead key is still answered with a success"
        );
        assert_eq!(mcp_follow_up_tool_count(&first_body), Some(1));
        assert_eq!(
            first_session_header, None,
            "the peer migration must not expose the new upstream session id: the \
             client's handle stays the proxy session id it presented"
        );
        assert_eq!(
            harness
                .peer_lookups
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "importing the peer binding costs exactly one peer lookup"
        );

        let first_calls = snapshot_mcp_migration_calls(&harness.call_log);
        assert_eq!(
            first_calls.len(),
            3,
            "rejected call, ONE re-initialize, ONE replay - never a loop: {:?}",
            first_calls
        );
        assert_eq!(first_calls[0].method, "tools/list");
        assert_eq!(first_calls[0].tavily_api_key, harness.dead_api_key);
        assert_eq!(
            first_calls[0].upstream_session_id_header.as_deref(),
            Some(MCP_KEY_MIGRATION_PINNED_SESSION),
            "the first attempt rode the dead binding the peer served"
        );
        assert_eq!(first_calls[1].method, "initialize");
        assert_eq!(first_calls[1].tavily_api_key, harness.spare_api_key);
        assert_eq!(first_calls[2].method, "tools/list");
        assert_eq!(first_calls[2].tavily_api_key, harness.spare_api_key);
        assert_eq!(
            first_calls[2].upstream_session_id_header.as_deref(),
            Some(MCP_KEY_MIGRATION_UPSTREAM_SESSION)
        );
        assert_eq!(
            first_calls
                .iter()
                .filter(|call| call.method == "initialize")
                .count(),
            1,
            "exactly one extra initialize per migrated request"
        );

        // The LOCAL row now carries the migration; the peer row still names the
        // dead key, which is why the next request must not migrate again.
        let (upstream_key_id, upstream_session_id, revoked_at) =
            fetch_mcp_session_upstream_binding(&pool, &harness.proxy_session_id).await;
        assert_eq!(
            upstream_key_id.as_deref(),
            Some(harness.spare_key_id.as_str())
        );
        assert_eq!(
            upstream_session_id.as_deref(),
            Some(MCP_KEY_MIGRATION_UPSTREAM_SESSION)
        );
        assert_eq!(revoked_at, None);

        let (second_status, second_body, _) = send_mcp_key_migration_follow_up(
            &client,
            &harness.url,
            &harness.proxy_session_id,
            "tools-after-peer-migration",
            false,
        )
        .await;
        assert_eq!(
            second_status, StatusCode::OK,
            "the client is always answered on the stable proxy session id"
        );
        assert_eq!(mcp_follow_up_tool_count(&second_body), Some(1));
        assert_eq!(
            harness
                .peer_lookups
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the migrated session rides the local row and stops asking the peer"
        );
        let second_calls = snapshot_mcp_migration_calls(&harness.call_log);
        assert_eq!(
            second_calls.len(),
            4,
            "the next request costs ONE upstream call, no second migration: {:?}",
            second_calls
        );
        assert_eq!(
            second_calls
                .iter()
                .filter(|call| call.method == "initialize")
                .count(),
            1,
            "a stale peer row can never turn into repeated re-initializes"
        );
        assert_eq!(second_calls[3].method, "tools/list");
        assert_eq!(second_calls[3].tavily_api_key, harness.spare_api_key);

        let _ = std::fs::remove_file(&harness.db_path);
    }

    /// Todo 14 case 6b: the same peer-owned session when the local copy of the row
    /// does NOT survive, which is the dual-active boundary the SPEC records as a
    /// known limitation - the peer row keeps naming the dead key, so every
    /// follow-up resolves through the peer and re-runs the migration chain. What
    /// this pins is the BOUND: one extra `initialize` and one replay PER REQUEST,
    /// a straight three-call chain each time, never a loop inside a request, and
    /// the client is answered with a success every time. It is a characterization
    /// pin - the deferred durable fix (a rows-affected return from the rebind)
    /// should change it deliberately.
    #[tokio::test]
    async fn mcp_follow_up_peer_row_that_keeps_the_dead_key_remigrates_per_request() {
        let harness = spawn_mcp_ha_peer_migration_harness("mcp-migrate-ha-peer-stale").await;
        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let client = Client::new();

        let (first_status, first_body, _) = send_mcp_key_migration_follow_up(
            &client,
            &harness.url,
            &harness.proxy_session_id,
            "tools-stale-peer-first",
            false,
        )
        .await;
        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(mcp_follow_up_tool_count(&first_body), Some(1));
        let first_calls = snapshot_mcp_migration_calls(&harness.call_log);
        assert_eq!(first_calls.len(), 3, "{:?}", first_calls);

        // The local copy does not survive, so the peer row - which still names the
        // dead key - stays the only binding the next request can resolve. A fenced
        // standby writer or a peer-sync overwrite is what does this in production;
        // deleting the row reproduces the observable state without asserting which
        // of them happened.
        sqlx::query("DELETE FROM mcp_sessions WHERE proxy_session_id = ?")
            .bind(&harness.proxy_session_id)
            .execute(&pool)
            .await
            .expect("drop the local copy of the peer-owned session row");

        let (second_status, second_body, _) = send_mcp_key_migration_follow_up(
            &client,
            &harness.url,
            &harness.proxy_session_id,
            "tools-stale-peer-second",
            false,
        )
        .await;
        assert_eq!(
            second_status, StatusCode::OK,
            "the client is answered again, not handed a reconnect error"
        );
        assert_eq!(mcp_follow_up_tool_count(&second_body), Some(1));
        assert_eq!(
            harness
                .peer_lookups
                .load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one peer lookup per request: the second request re-imports the peer row"
        );

        let calls = snapshot_mcp_migration_calls(&harness.call_log);
        assert_eq!(
            calls.len(),
            6,
            "two bounded three-call chains, never a loop: {:?}",
            calls
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == "initialize")
                .count(),
            2,
            "exactly ONE extra initialize per request, no matter how often the peer \
             row keeps the dead key"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == "tools/list")
                .count(),
            4,
            "one rejected call plus one replay per request"
        );
        // The second request's own chain, in order: dead key rejected, ONE
        // re-initialize on the spare key, ONE replay on the migrated session.
        assert_eq!(calls[3].method, "tools/list");
        assert_eq!(calls[3].tavily_api_key, harness.dead_api_key);
        assert_eq!(
            calls[3].upstream_session_id_header.as_deref(),
            Some(MCP_KEY_MIGRATION_PINNED_SESSION),
            "the second request rode the dead binding the peer still serves"
        );
        assert_eq!(calls[4].method, "initialize");
        assert_eq!(calls[4].tavily_api_key, harness.spare_api_key);
        assert_eq!(calls[5].method, "tools/list");
        assert_eq!(calls[5].tavily_api_key, harness.spare_api_key);
        assert_eq!(
            calls[5].upstream_session_id_header.as_deref(),
            Some(MCP_KEY_MIGRATION_UPSTREAM_SESSION)
        );

        // The re-imported row was rebound again, and it is live: nothing in the
        // repeated chain ever revokes the session or answers the client with a 502.
        let (upstream_key_id, upstream_session_id, revoked_at) =
            fetch_mcp_session_upstream_binding(&pool, &harness.proxy_session_id).await;
        assert_eq!(
            upstream_key_id.as_deref(),
            Some(harness.spare_key_id.as_str())
        );
        assert_eq!(
            upstream_session_id.as_deref(),
            Some(MCP_KEY_MIGRATION_UPSTREAM_SESSION)
        );
        assert_eq!(revoked_at, None);

        let _ = std::fs::remove_file(&harness.db_path);
    }
