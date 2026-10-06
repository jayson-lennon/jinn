// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Admission asks for turn dispatch.
//!
//! Every dispatch path asks the phase actor to begin its stream before
//! doing anything else. A refusal means the generation the dispatch
//! would have joined is dead — a cancel landed while the turn sat
//! queued — and the caller publishes nothing downstream of the ask.

use jinn_core_types::SessionId;
use jinn_kernel::common::services::Services;
use jinn_session_msg::phase_command::{DispatchKind, PhaseCommand, PhaseDecision};

/// Asks the phase actor to admit a stream start for `session_id`.
///
/// A dead phase actor is surfaced as a refusal with an error log rather
/// than a panic: a wedged ask must not take the dispatch path down, and
/// the actor is spawned at composition before any publisher can run.
async fn ask_begin_stream(
    services: &Services,
    session_id: &SessionId,
    kind: DispatchKind,
    dispatched_at: jiff::Timestamp,
) -> Option<PhaseDecision> {
    match jinn_kernel::common::phase_command::apply_phase(
        services,
        PhaseCommand::BeginStream {
            session_id: session_id.clone(),
            kind,
            dispatched_at,
        },
    )
    .await
    {
        Ok(decision) => Some(decision),
        Err(report) => {
            tracing::error!(
                session_id = %session_id,
                ?report,
                "phase admission ask failed; refusing the dispatch"
            );
            None
        }
    }
}

/// Admits or refuses a stream start, treating every failure as refusal.
///
/// The one call sites make: `admitted` is the whole gate.
pub(crate) async fn admit_begin_stream(
    services: &Services,
    session_id: &SessionId,
    kind: DispatchKind,
    dispatched_at: jiff::Timestamp,
) -> PhaseDecision {
    ask_begin_stream(services, session_id, kind, dispatched_at)
        .await
        .unwrap_or(PhaseDecision::refused())
}
