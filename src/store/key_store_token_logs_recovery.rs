/// Reason code of the NaturalDay sweep (keenable-integration todo 11). Kept
/// additive to the closed Tavily-era vocabulary in lib.rs.
const NATURAL_DAY_RECOVERY_REASON_CODE: &str = "natural_day_recovery";

impl KeyStore {
    /// Renewal-date recovery for SerpApi (`RenewalDate`). Keeps the exact
    /// pre-todo-11 bulk UPDATE: SerpApi's snapshot-scoped audit row is written
    /// by the quota-sync adapter, so this sweep stays audit-neutral.
    ///
    /// Flips exhausted -> active only once the per-key `quota_renews_at` is
    /// reached, i.e. `status_changed_at < quota_renews_at <= now`. Keys with
    /// an unknown renewal date stay exhausted.
    pub(crate) async fn recover_keys_by_renewal_date(
        &self,
        provider: crate::providers::ProviderId,
        now_ts: i64,
    ) -> Result<(), ProxyError> {
        sqlx::query(
            r#"
            UPDATE api_keys
            SET status = ?, status_changed_at = ?
            WHERE status = ?
              AND provider = ?
              AND quota_renews_at IS NOT NULL
              AND quota_renews_at <= ?
              AND status_changed_at IS NOT NULL
              AND status_changed_at < quota_renews_at
            "#,
        )
        .bind(STATUS_ACTIVE)
        .bind(now_ts)
        .bind(STATUS_EXHAUSTED)
        .bind(provider.as_str())
        .bind(now_ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// NaturalDay recovery (Zhihu, keenable-integration todo 11).
    ///
    /// Same boundary predicate as [`Self::recover_keys_by_renewal_date`] —
    /// `status_changed_at < quota_renews_at <= now` — but the due keys are
    /// selected first so each flip carries the `auto_restore_active` audit
    /// row. A UTC month boundary never matches here: only `quota_renews_at`,
    /// the next Asia/Shanghai midnight, drives the flip.
    pub(crate) async fn recover_natural_day_keys(
        &self,
        provider: crate::providers::ProviderId,
        now_ts: i64,
    ) -> Result<(), ProxyError> {
        let due: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT id
            FROM api_keys
            WHERE status = ?
              AND provider = ?
              AND quota_renews_at IS NOT NULL
              AND quota_renews_at <= ?
              AND status_changed_at IS NOT NULL
              AND status_changed_at < quota_renews_at
            "#,
        )
        .bind(STATUS_EXHAUSTED)
        .bind(provider.as_str())
        .bind(now_ts)
        .fetch_all(&self.pool)
        .await?;

        let mut flipped = Vec::with_capacity(due.len());
        for key_id in due {
            let changed = sqlx::query(
                r#"
                UPDATE api_keys
                SET status = ?, status_changed_at = ?
                WHERE id = ? AND status = ?
                "#,
            )
            .bind(STATUS_ACTIVE)
            .bind(now_ts)
            .bind(&key_id)
            .bind(STATUS_EXHAUSTED)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if changed > 0 {
                flipped.push(key_id);
            }
        }
        self.record_natural_day_restore_records(provider, &flipped, now_ts)
            .await
    }

    /// By-id twin of `mark_quota_exhausted` for health seams that hold the
    /// public key id (the `NaturalDay` quota-sample path). The todo-12 Zhihu
    /// adapter is its production caller.
    pub(crate) async fn mark_quota_exhausted_by_id(&self, key_id: &str) -> Result<bool, ProxyError> {
        let now = self.backend_time.now_ts();
        let res = sqlx::query(
            r#"
            UPDATE api_keys
            SET status = ?, status_changed_at = ?, last_used_at = ?
            WHERE id = ? AND status NOT IN (?, ?) AND deleted_at IS NULL
            "#,
        )
        .bind(STATUS_EXHAUSTED)
        .bind(now)
        .bind(now)
        .bind(key_id)
        .bind(STATUS_DISABLED)
        .bind(STATUS_EXHAUSTED)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Writes one `auto_restore_active` audit row per NaturalDay flip
    /// (keenable-integration todo 11).
    async fn record_natural_day_restore_records(
        &self,
        provider: crate::providers::ProviderId,
        key_ids: &[String],
        now_ts: i64,
    ) -> Result<(), ProxyError> {
        for key_id in key_ids {
            let snapshot = self.fetch_key_state_snapshot(key_id).await?;
            self.insert_api_key_maintenance_record(ApiKeyMaintenanceRecord {
                id: nanoid!(12),
                key_id: key_id.clone(),
                source: MAINTENANCE_SOURCE_SYSTEM.to_string(),
                operation_code: MAINTENANCE_OP_AUTO_RESTORE_ACTIVE.to_string(),
                operation_summary: "自动恢复为 active".to_string(),
                reason_code: Some(NATURAL_DAY_RECOVERY_REASON_CODE.to_string()),
                reason_summary: Some("自然日额度重置，恢复 active".to_string()),
                reason_detail: Some(format!(
                    "provider={} quota_renews_at<=now({now_ts}) source=reset_monthly",
                    provider.as_str()
                )),
                request_log_id: None,
                auth_token_log_id: None,
                auth_token_id: None,
                actor_user_id: None,
                actor_display_name: None,
                status_before: Some(STATUS_EXHAUSTED.to_string()),
                status_after: snapshot.status,
                quarantine_before: snapshot.quarantined,
                quarantine_after: snapshot.quarantined,
                created_at: now_ts,
            })
            .await?;
        }
        Ok(())
    }
}
