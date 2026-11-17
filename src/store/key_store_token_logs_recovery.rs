impl KeyStore {
    /// Renewal-date recovery shared by SerpApi (`RenewalDate`) and Zhihu
    /// (`NaturalDay`).
    ///
    /// Flips exhausted -> active only once the per-key `quota_renews_at` is
    /// reached, i.e. `status_changed_at < quota_renews_at <= now`. The
    /// boundary semantics are identical for both policies: Zhihu stores the
    /// next Asia/Shanghai midnight as epoch seconds, and neither provider ever
    /// recovers on a UTC month boundary. Keys with an unknown renewal date
    /// stay exhausted.
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
}
