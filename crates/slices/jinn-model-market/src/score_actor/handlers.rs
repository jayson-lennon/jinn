//! The score actor's handlers — one named function per trigger, plus
//! the two independent load pipelines they run (one per source, each
//! with its own error path).

use std::sync::Arc;

use jinn_kernel::common::actor_deps::BusPublish as _;
use jinn_model_market_msg::EciLoaded;
use jinn_model_market_msg::EciSource;
use jinn_model_market_msg::RefreshFailed;
use jinn_model_market_msg::ValsLoaded;
use jinn_model_market_msg::ValsSource;

use super::ScoreMarketActor;

impl ScoreMarketActor {
    /// `RefreshScores`: serve the fresh caches, else download, ECI seed
    /// standing behind an ECI failure.
    pub(super) async fn handle_refresh_scores(&mut self) {
        run_eci_load(self, false).await;
        run_vals_load(self, false).await;
    }

    /// The forced refresh: drop both cache dirs unconditionally, then
    /// run the same load pipelines forced to download.
    ///
    /// Two triggers reach this body, and both mean the same thing — the
    /// caches must go and the data must be refetched: the
    /// `RefreshScoresForce` command, and the `MarketModelsRefreshed`
    /// event the models actor emits when the models half of `<c-r>` is
    /// done.
    ///
    /// A failed cache drop is reported against its own source and does
    /// not skip either download attempt.
    pub(super) async fn handle_force_refresh(&mut self) {
        if let Err(report) = crate::score_cache::drop_cache_dir(&self.cache_dirs.eci) {
            self.publish(RefreshFailed {
                what: "eci".to_owned(),
                message: report.to_string(),
            })
            .await;
        }
        if let Err(report) = crate::score_cache::drop_cache_dir(&self.cache_dirs.vals) {
            self.publish(RefreshFailed {
                what: "vals".to_owned(),
                message: report.to_string(),
            })
            .await;
        }
        run_eci_load(self, true).await;
        run_vals_load(self, true).await;
    }

    /// Writes a successful ECI load into the data cell and announces it.
    pub(super) async fn finish_eci_load(
        &self,
        data: crate::eci::EciData,
        source: EciSource,
        fetched_at_ms: u64,
    ) {
        let stamp =
            jiff::Timestamp::from_millisecond(i64::try_from(fetched_at_ms).unwrap_or(i64::MAX))
                .unwrap_or(jiff::Timestamp::MAX);
        let matched = u32::try_from(data.index.len()).unwrap_or(u32::MAX);
        let total = data.total_rows;
        self.data_cell.update(|state| {
            state.eci = Some(Arc::new(data.index));
            state.eci_fetched_at = Some(stamp);
            state.eci_loading = false;
            state.eci_source = source;
        });
        self.publish(EciLoaded {
            source,
            matched,
            total,
        })
        .await;
    }

    /// Writes an ECI failure into the data cell (the seed data stands)
    /// and announces it.
    pub(super) async fn fail_eci_load(&self, message: String) {
        self.data_cell.update(|state| {
            state.eci_loading = false;
            state.eci_source = EciSource::Seed;
            state.last_error = Some(message.clone());
        });
        self.publish(RefreshFailed {
            what: "eci".to_owned(),
            message,
        })
        .await;
    }

    /// Writes a successful vals load into the data cell and announces
    /// it.
    pub(super) async fn finish_vals_load(
        &self,
        index: jinn_model_market_msg::ValsIndex,
        total: u32,
        fetched_at_ms: u64,
    ) {
        let stamp =
            jiff::Timestamp::from_millisecond(i64::try_from(fetched_at_ms).unwrap_or(i64::MAX))
                .unwrap_or(jiff::Timestamp::MAX);
        let matched = u32::try_from(index.len()).unwrap_or(u32::MAX);
        self.data_cell.update(|state| {
            state.vals = Some(Arc::new(index));
            state.vals_fetched_at = Some(stamp);
            state.vals_loading = false;
            state.vals_source = ValsSource::Download;
        });
        self.publish(ValsLoaded {
            source: ValsSource::Download,
            matched,
            total,
        })
        .await;
    }

    /// Writes a vals failure into the data cell and announces it.
    ///
    /// Only the vals fields move — the ECI index, the rows, and detail
    /// mode stay exactly as they were, so a vals outage degrades only
    /// the VALS column.
    pub(super) async fn fail_vals_load(&self, message: String) {
        self.data_cell.update(|state| {
            state.vals_loading = false;
            state.last_error = Some(message.clone());
        });
        self.publish(RefreshFailed {
            what: "vals".to_owned(),
            message,
        })
        .await;
    }
}

/// The ECI load pipeline: fresh cache → (forced) download → seed.
///
/// `force` is the forced refresh's mandate: skip the fresh-cache read
/// and go straight to the network, so a refresh actually refreshes.
async fn run_eci_load(actor: &ScoreMarketActor, force: bool) {
    actor.data_cell.update(|state| state.eci_loading = true);
    let now = (actor.now_ms)();

    if !force
        && let Some(cached) =
            crate::score_cache::load_eci_fresh(&actor.cache_dirs.eci, super::SCORE_TTL_MS, || {
                (actor.now_ms)()
            })
    {
        eci_from_bytes(actor, cached.files, now, EciSource::Download).await;
        return;
    }

    match crate::score_cache::fetch_bytes(&actor.client, &actor.zip_url).await {
        Ok(zip_bytes) => {
            if let Err(report) =
                crate::score_cache::download_and_extract(&actor.cache_dirs.eci, &zip_bytes, now)
            {
                // Downloaded but unusable: fall back to the seed and say
                // so — never leave the cell loading, never write a
                // partial extraction.
                fail_to_seed(
                    actor,
                    format!("ECI refresh failed; showing shipped seed: {report}"),
                )
                .await;
                return;
            }
            match crate::score_cache::load_eci_any(&actor.cache_dirs.eci) {
                Some(cached) => {
                    eci_from_bytes(actor, cached.files, now, EciSource::Download).await;
                }
                None => {
                    fail_to_seed(
                        actor,
                        "ECI refresh failed; showing shipped seed: extraction vanished".to_owned(),
                    )
                    .await;
                }
            }
        }
        Err(report) => {
            // No network (or a failing origin): the seed stands. The ECI
            // column still populates with eci_source = Seed.
            fail_to_seed(
                actor,
                format!("ECI refresh failed; showing shipped seed: {report}"),
            )
            .await;
        }
    }
}

/// The vals load pipeline: fresh cache → (forced) download — and on
/// failure, nothing: no seed exists for vals, so the column stays blank.
///
/// The URL is re-derived from the live page every refresh; the payload's
/// `_astro` path is content-hashed per deploy, so a persisted path would
/// be a dead URL. The pipeline touches only the cell's vals fields — a
/// vals failure must never blank the ECI column.
async fn run_vals_load(actor: &ScoreMarketActor, force: bool) {
    actor.data_cell.update(|state| state.vals_loading = true);
    let now = (actor.now_ms)();

    if !force
        && let Some(cached) =
            crate::score_cache::load_vals_fresh(&actor.cache_dirs.vals, super::SCORE_TTL_MS, || {
                (actor.now_ms)()
            })
    {
        vals_from_bytes(actor, cached.bytes, now).await;
        return;
    }

    match crate::vals::fetch_index_json(&actor.client, &actor.vals_page_url).await {
        Ok(json_bytes) => {
            if let Err(report) =
                crate::score_cache::store_vals(&actor.cache_dirs.vals, &json_bytes, now)
            {
                actor
                    .fail_vals_load(format!("Vals refresh failed: {report}"))
                    .await;
                return;
            }
            vals_from_bytes(actor, json_bytes, now).await;
        }
        Err(report) => {
            actor
                .fail_vals_load(format!("Vals refresh failed: {report}"))
                .await;
        }
    }
}

/// Parses cached-or-downloaded vals JSON and lands a successful load in
/// the cell; a parse failure reports without touching the ECI fields.
async fn vals_from_bytes(actor: &ScoreMarketActor, bytes: Vec<u8>, fetched_at_ms: u64) {
    match crate::vals::parse_vals_index(&bytes) {
        Ok(index) => {
            let total = u32::try_from(index.len()).unwrap_or(u32::MAX);
            actor.finish_vals_load(index, total, fetched_at_ms).await;
        }
        Err(report) => {
            actor
                .fail_vals_load(format!("Vals data unusable: {report}"))
                .await;
        }
    }
}

/// Reports a failed ECI refresh and lands the compiled seed in the cell,
/// so the ECI column still populates (`first_run_without_network_uses_seed`).
async fn fail_to_seed(actor: &ScoreMarketActor, message: String) {
    actor.fail_eci_load(message).await;
    super::load_seed(actor).await;
}

/// Parses the three CSVs and hands a successful ECI load to the actor.
async fn eci_from_bytes(
    actor: &ScoreMarketActor,
    files: Vec<Vec<u8>>,
    fetched_at_ms: u64,
    source: EciSource,
) {
    let mut files = files.into_iter();
    let eci_scores = files.next().unwrap_or_default();
    let benchmark_metadata = files.next().unwrap_or_default();
    let model_metadata = files.next().unwrap_or_default();
    match crate::eci::parse_all(&eci_scores, &benchmark_metadata, &model_metadata) {
        Ok(data) => actor.finish_eci_load(data, source, fetched_at_ms).await,
        Err(report) => {
            fail_to_seed(
                actor,
                format!(
                    "ECI data unusable; showing shipped seed: {}",
                    parse_failure_detail(&report)
                ),
            )
            .await;
        }
    }
}

/// Renders a parse-error report compactly for the status line.
fn parse_failure_detail(report: &error_stack::Report<crate::eci::EciParseError>) -> String {
    format!("{report}")
}
