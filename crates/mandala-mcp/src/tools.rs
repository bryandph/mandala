//! The 12 fleet tools: argument structs + agent-facing descriptions.
//!
//! Each struct is one tool's argument schema (`#[mcp_tool]` + `JsonSchema`
//! derive). The `description` strings are the Python FastMCP tool docstrings
//! ported VERBATIM — they are agent-facing documentation, part of the parity
//! contract (`fleet-mcp` spec: same tool names, argument names, defaults).
//! Defaulted arguments are `Option<T>` (optional in the schema, defaulted at
//! dispatch), mirroring the Python keyword defaults.

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::Tool;

/// `members` arguments.
#[mcp_tool(
    name = "members",
    description = concat!(
        "Every fleet member (NixOS + facts-only). Compact by default —\n",
        "platform, arch, category, role, tags, surfaces (a=ansible\n",
        "d=deploy-rs s=sops) per member — because the full aggregate dump\n",
        "is tens of KB and blows client tool-result caps. `full=true`\n",
        "returns everything; `host_eval` gives one member's full record."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct MembersTool {
    /// Return the full member records instead of the compact view.
    #[json_schema(default = false)]
    pub full: Option<bool>,
}

/// `groups` arguments (none).
#[mcp_tool(
    name = "groups",
    description = concat!(
        "Taxonomy groups and their member names — the `@group` fan-out\n",
        "spelling shared by deploy, ansible `-l`, and `deployBatch`."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct GroupsTool {}

/// `resolve` arguments.
#[mcp_tool(
    name = "resolve",
    description = concat!(
        "Expand a selector (`@group`, a member, or a comma-list) —\n",
        "identical to `mandala resolve` and the `--limit` set a deploy would\n",
        "fan out to. Returns the sorted `members` plus the comma-joined\n",
        "`limit` string, which is exactly the `confirm` value the gated\n",
        "actions (deploy, reboot, restart_service) require."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct ResolveTool {
    /// The selector to expand.
    pub selector: String,
}

/// `ping` arguments.
#[mcp_tool(
    name = "ping",
    description = concat!(
        "Ansible reachability probe over a resolved selector — per-host\n",
        "reachable/unreachable. Read-only: it changes no fleet state. The raw\n",
        "ansible output is included so an unreachable host can be debugged.\n",
        "`forks` probes hosts concurrently and `connect_timeout` (seconds)\n",
        "caps each ssh attempt, so a whole-fleet probe with a few dead\n",
        "workstations finishes inside a client's tool-call budget instead\n",
        "of serially waiting out every unreachable host."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct PingTool {
    /// The selector to probe.
    pub selector: String,
    /// Concurrent probe fan-out (`ansible -f`).
    #[json_schema(default = 15)]
    pub forks: Option<i64>,
    /// Per-host ssh connect timeout in seconds (`ansible -T`).
    #[json_schema(default = 10)]
    pub connect_timeout: Option<i64>,
}

/// `host_eval` arguments.
#[mcp_tool(
    name = "host_eval",
    description = concat!(
        "Per-host eval information. Aggregate metadata is always returned;\n",
        "the evaluated system `toplevel` out-path is computed only when\n",
        "`toplevel=true` (one slow nix eval). A failed eval is returned as a\n",
        "structured `eval_error`, not raised, so the client can debug it."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct HostEvalTool {
    /// The member to report.
    pub member: String,
    /// Also evaluate the system toplevel out-path (one slow nix eval).
    #[json_schema(default = false)]
    pub toplevel: Option<bool>,
}

/// `drift` arguments.
#[mcp_tool(
    name = "drift",
    description = concat!(
        "Deployed-generation drift (contract vs reported state) — the same\n",
        "exact-out-path judgement `mandala drift` makes. A plain read uses\n",
        "existing snapshots and the rev-keyed expectation cache: no survey, no\n",
        "eval. `refresh` runs the read-only state survey first; `do_eval`\n",
        "re-evaluates the expected toplevels (one slow nix eval). An eval\n",
        "failure is returned as a structured `eval_error`, not raised.\n",
        "\n",
        "The result always carries a `summary` ({status: count} over the whole\n",
        "fleet) and `total`; `statuses` filters the `entries` list to just\n",
        "those statuses (e.g. `[\"drift\", \"unreachable\"]`) so one noisy status\n",
        "— every host goes `reboot-pending` after a kernel bump — doesn't\n",
        "drown the rest. `reboot-pending` fires only on a boot-critical\n",
        "change between booted and current (kernel, kernel-modules, initrd,\n",
        "kernel-params); an activated-but-unrebooted generation with none\n",
        "of those reports `activated` instead. `expected_source: \"none\"` means NO expected-toplevel\n",
        "comparison happened (no cache for this rev and `do_eval` false):\n",
        "current-vs-expected judgements are then absent, not clean."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct DriftTool {
    /// Run the read-only state survey first.
    #[json_schema(default = false)]
    pub refresh: Option<bool>,
    /// Re-evaluate the expected toplevels (one slow nix eval).
    #[json_schema(default = false)]
    pub do_eval: Option<bool>,
    /// Filter `entries` to just these statuses (summary stays whole-fleet).
    pub statuses: Option<Vec<String>>,
}

/// `reload` arguments (none).
#[mcp_tool(
    name = "reload",
    description = concat!(
        "Re-read the fleet contract: evaluate a FRESH inventory aggregate\n",
        "(the one slow `nix eval .#mandala`) and swap it in as what every\n",
        "other tool serves. Use after the contract changes — a member added,\n",
        "tags moved, groups reshaped — because the aggregate is otherwise\n",
        "cached for the life of the server. Hosted in the TUI, the swap\n",
        "refreshes the operator's tables too, exactly like pressing `r`."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct ReloadTool {}

/// `deploy_status` arguments.
#[mcp_tool(
    name = "deploy_status",
    description = concat!(
        "Live and recent run state from the shared run registry — EVERY\n",
        "registered run kind (deploys and command runs like reboot), from\n",
        "any frontend (TUI, CLI, or this server). With a `run_id`, report\n",
        "just that run; otherwise the most recent `limit` runs — which also\n",
        "surfaces orphaned/lingering runs (`liveness: running` with an old\n",
        "`started_at`). Deploy runs report per-host states from the\n",
        "protocol's sticky terminal states, so a confirmed host stays\n",
        "confirmed and a rolled-back host stays flagged; `milestones` is the\n",
        "raw per-host event sequence — repeats (activate, wait, activate, …)\n",
        "are genuine re-entries from the engine, not display noise. `phase`\n",
        "summarizes where a live deploy is: `batch-build` (play 1, no\n",
        "per-host events yet) → `fan-out` → `done`. Command runs report\n",
        "liveness (pid, then the reaped exit code in `meta.rc`) plus the\n",
        "tail of their teed `output.log`.\n",
        "\n",
        "`wait_seconds` (with a `run_id`) blocks until the run leaves the\n",
        "`running` state or the wait elapses — one call instead of a poll\n",
        "loop; capped at 570s to stay under client timeouts. The returned\n",
        "`liveness` tells whether it finished or the wait timed out."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct DeployStatusTool {
    /// Report just this run (else the most recent `limit` runs).
    pub run_id: Option<String>,
    /// How many recent runs to list when no `run_id` is given.
    #[json_schema(default = 10)]
    pub limit: Option<i64>,
    /// Block until the run settles or the wait elapses (cap 570).
    #[json_schema(default = 0)]
    pub wait_seconds: Option<i64>,
}

/// `build` arguments.
#[mcp_tool(
    name = "build",
    description = concat!(
        "Build the resolved members' system `toplevel`(s) with `nix build` —\n",
        "WITHOUT activating anything (local store only), so no confirmation is\n",
        "required. Launches as a REGISTERED BACKGROUND RUN (a cold multi-host\n",
        "build outlives any client timeout) and waits up to `wait_seconds`\n",
        "(cap 570) for it to finish: a finished build returns `ok` +\n",
        "`out_paths` (or the failing output), a still-running one returns\n",
        "`building: true` — follow with `deploy_status(run_id,\n",
        "wait_seconds=…)`. Output streams to the returned `log` either way."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct BuildTool {
    /// The selector whose members to build.
    pub selector: String,
    /// How long to wait for the build before returning `building: true`.
    #[json_schema(default = 120)]
    pub wait_seconds: Option<i64>,
}

/// `deploy` arguments.
#[mcp_tool(
    name = "deploy",
    description = concat!(
        "Deploy the resolved members through the deploy playbook — the\n",
        "engine: `--limit`, throttle, and deploy-rs magic rollback are never\n",
        "bypassed. Defaults to dry-activate (build + copy, no switch). A REAL\n",
        "activation (`dry_activate=false`) requires `confirm` to equal the\n",
        "resolved `--limit` target — take it from `resolve`'s `limit` field, a\n",
        "prior run's `limit`, or this tool's refusal (`required_confirm`) —\n",
        "else it refuses WITHOUT launching. Returns the run id; follow with\n",
        "`deploy_status` (its `wait_seconds` blocks until the run settles)."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct DeployTool {
    /// The selector to deploy.
    pub selector: String,
    /// Build + copy only, no switch (the ungated default).
    #[json_schema(default = true)]
    pub dry_activate: Option<bool>,
    /// For a real activation: must equal the resolved `--limit` target.
    pub confirm: Option<String>,
}

/// `restart_service` arguments.
#[mcp_tool(
    name = "restart_service",
    description = concat!(
        "Restart one systemd unit on the resolved members via ad-hoc\n",
        "ansible (`systemd_service state=restarted`) — the middle ground\n",
        "between a full deploy (no-op when the closure hasn't changed) and a\n",
        "reboot (far too big a hammer for picking up a service-level change,\n",
        "e.g. k3s re-reading registries.yaml). `forks` bounds how many hosts\n",
        "restart AT ONCE — a concurrency cap, NOT a rolling gate: there is\n",
        "no fail-fast or health check between batches, so every resolved\n",
        "host is eventually restarted even if the first batch breaks.\n",
        "\n",
        "Mutating, so it takes the deploy/reboot confirm gate: `confirm` must\n",
        "equal the resolved `--limit` target (the `limit` field of `resolve`),\n",
        "else it refuses WITHOUT running. Unit names are validated to a plain\n",
        "systemd name — no paths, no shell."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct RestartServiceTool {
    /// The selector whose members restart the unit.
    pub selector: String,
    /// The systemd unit to restart (plain name only).
    pub unit: String,
    /// Must equal the resolved `--limit` target.
    pub confirm: Option<String>,
    /// How many hosts restart at once (a concurrency cap, not a gate).
    #[json_schema(default = 4)]
    pub forks: Option<i64>,
}

/// `reboot` arguments.
#[mcp_tool(
    name = "reboot",
    description = concat!(
        "Reboot the resolved members through the reboot playbook with the\n",
        "serial-order (`1` serial, `2` rolling, `100%` all-at-once) and k8s\n",
        "`drain` options the TUI offers. Requires `confirm` to equal the\n",
        "resolved `--limit` target, else it refuses WITHOUT launching. Prefers\n",
        "the operator's `ans-reboot` wrapper (which carries the env the k8s\n",
        "drain needs), falling back to `playbooks/reboot.yaml`.\n",
        "\n",
        "Launches as a REGISTERED BACKGROUND RUN and returns `run_id` at\n",
        "once — a rolling reboot far outlives any client timeout, and this\n",
        "way the run stays observable (TUI, `deploy_status`) instead of\n",
        "orphaning. Follow with `deploy_status(run_id, wait_seconds=…)`;\n",
        "the playbook output streams to the returned `log` file either way."
    )
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct RebootTool {
    /// The selector to reboot.
    pub selector: String,
    /// Reboot batch order: a count or a percentage (`1`, `2`, `100%`).
    #[json_schema(default = "1")]
    pub serial: Option<String>,
    /// Drain k8s nodes before rebooting them.
    #[json_schema(default = true)]
    pub drain: Option<bool>,
    /// Must equal the resolved `--limit` target.
    pub confirm: Option<String>,
}

/// The full tool surface, in the Python server's registration order — what
/// `tools/list` advertises.
#[must_use]
pub fn all_tools() -> Vec<Tool> {
    vec![
        MembersTool::tool(),
        GroupsTool::tool(),
        ResolveTool::tool(),
        PingTool::tool(),
        HostEvalTool::tool(),
        DriftTool::tool(),
        ReloadTool::tool(),
        DeployStatusTool::tool(),
        BuildTool::tool(),
        DeployTool::tool(),
        RestartServiceTool::tool(),
        RebootTool::tool(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_tools_in_registration_order() {
        let names: Vec<String> = all_tools().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "members",
                "groups",
                "resolve",
                "ping",
                "host_eval",
                "drift",
                "reload",
                "deploy_status",
                "build",
                "deploy",
                "restart_service",
                "reboot",
            ]
        );
    }

    #[test]
    fn gated_actions_take_a_confirm_argument() {
        for tool in [
            DeployTool::tool(),
            RestartServiceTool::tool(),
            RebootTool::tool(),
        ] {
            assert!(
                tool.input_schema
                    .properties
                    .as_ref()
                    .is_some_and(|p| p.contains_key("confirm")),
                "{} lacks confirm",
                tool.name
            );
        }
    }
}
