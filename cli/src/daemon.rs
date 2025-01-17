// SPDX-FileCopyrightText: 2021 Chorus One AG
// SPDX-License-Identifier: GPL-3.0

//! Maintenance daemon that periodically executes maintenance tasks, and serves metrics.
//!
//! The daemon consists of two parts: a main loop, and http server threads. The
//! main loop polls the latest state from the chain through the normal RPC, and
//! executes maintenance tasks if needed. It also publishes a snapshot of its
//! most recently seen Solido state in an `Arc` so the http threads can serve it
//! without blocking the main loop.

use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use rand::Rng;
use tiny_http::{Request, Response, Server};

use crate::config::RunMaintainerOpts;
use crate::error::AsPrettyError;
use crate::maintenance::{try_perform_maintenance, MaintenanceOutput, SolidoState};
use crate::prometheus::{write_metric, Metric, MetricFamily};
use crate::SnapshotClientConfig;

/// Metrics counters that track how many maintenance operations we performed.
#[derive(Clone)]
struct MaintenanceMetrics {
    /// Number of times that we checked if there was maintenance to perform.
    polls: u64,

    /// Number of times that we tried to perform maintenance, but encountered an error.
    errors: u64,

    /// Number of times we performed `StakeDeposit`.
    transactions_stake_deposit: u64,

    /// Number of times we performed `UpdateExchangeRate`.
    transactions_update_exchange_rate: u64,

    /// Number of times we performed `WithdrawInactiveStake`.
    transactions_withdraw_inactive_stake: u64,

    /// Number of times we performed `CollectValidatorFee`
    transactions_collect_validator_fee: u64,

    /// Number of times we performed a `MergeStake`.
    transactions_merge_stake: u64,

    /// Number of times we performed `ClaimValidatorFee`.
    transactions_claim_validator_fee: u64,
    // TODO(#96#issuecomment-859388866): Track how much the daemon spends on transaction fees,
    // so we know how much SOL it costs to operate.
    // spent_lamports_total: u64
    /// Number of times we performed `UnstakeFromInactiveValidator`.
    transactions_unstake_from_inactive_validator: u64,

    /// Number of times we performed `RemoveValidator`.
    transactions_remove_validator: u64,
}

impl MaintenanceMetrics {
    /// Serialize metrics in Prometheus text format.
    pub fn write_prometheus<W: io::Write>(&self, out: &mut W) -> io::Result<()> {
        write_metric(
            out,
            &MetricFamily {
                name: "solido_maintenance_polls_total",
                help:
                    "Number of times we checked if there is maintenance to perform, since launch.",
                type_: "counter",
                metrics: vec![Metric::new(self.polls)],
            },
        )?;
        write_metric(out, &MetricFamily {
            name: "solido_maintenance_errors_total",
            help: "Number of times we encountered an error while trying to perform maintenance, since launch.",
            type_: "counter",
            metrics: vec![Metric::new(self.errors)]
        })?;
        write_metric(
            out,
            &MetricFamily {
                name: "solido_maintenance_transactions_total",
                help: "Number of maintenance transactions executed, since launch.",
                type_: "counter",
                metrics: vec![
                    Metric::new(self.transactions_stake_deposit)
                        .with_label("operation", "StakeDeposit".to_string()),
                    Metric::new(self.transactions_update_exchange_rate)
                        .with_label("operation", "UpdateExchangeRate".to_string()),
                    Metric::new(self.transactions_withdraw_inactive_stake)
                        .with_label("operation", "WithdrawInactiveStake".to_string()),
                    Metric::new(self.transactions_collect_validator_fee)
                        .with_label("operation", "CollectValidatorFee".to_string()),
                    Metric::new(self.transactions_merge_stake)
                        .with_label("operation", "MergeStake".to_string()),
                    Metric::new(self.transactions_claim_validator_fee)
                        .with_label("operation", "ClaimValidatorFee".to_string()),
                    Metric::new(self.transactions_unstake_from_inactive_validator)
                        .with_label("operation", "UnstakeFromInactiveValidator".to_string()),
                    Metric::new(self.transactions_remove_validator)
                        .with_label("operation", "RemoveValidator".to_string()),
                ],
            },
        )?;
        Ok(())
    }
}

/// Snapshot of metrics and Solido state.
struct Snapshot {
    /// Metrics about what the daemon has done so far.
    metrics: MaintenanceMetrics,

    /// The current state of on-chain accounts, and the time at which we obtained
    /// that data.
    solido: Option<SolidoState>,
}

/// Mutex that holds the latest snapshot.
///
/// At startup it holds None, after that it will always hold Some Arc.
/// To read the current snapshot, we only have to lock the mutex briefly,
/// so we can clone the arc, and then we can continue to work with that
/// snapshot without any lock. This holds for publishing a new state as well:
/// we can prepare it privately, and we only need to lock the mutex briefly
/// to swap the Arc.
type SnapshotMutex = Mutex<Option<Arc<Snapshot>>>;

/// Run the maintenance loop.
fn run_main_loop(
    config: &mut SnapshotClientConfig,
    opts: &RunMaintainerOpts,
    snapshot_mutex: &SnapshotMutex,
) {
    let mut metrics = MaintenanceMetrics {
        polls: 0,
        errors: 0,
        transactions_stake_deposit: 0,
        transactions_update_exchange_rate: 0,
        transactions_withdraw_inactive_stake: 0,
        transactions_collect_validator_fee: 0,
        transactions_merge_stake: 0,
        transactions_claim_validator_fee: 0,
        transactions_unstake_from_inactive_validator: 0,
        transactions_remove_validator: 0,
    };
    let mut rng = rand::thread_rng();

    loop {
        metrics.polls += 1;
        let mut do_wait = false;

        let result = config.with_snapshot(|mut config| {
            let state =
                SolidoState::new(&mut config, opts.solido_program_id(), opts.solido_address())?;

            match try_perform_maintenance(&mut config, &state)? {
                None => {
                    // Nothing to be done, try again later.
                    do_wait = true;
                }
                Some(maintenance_output) => {
                    println!("{}", maintenance_output);
                    match maintenance_output {
                        MaintenanceOutput::StakeDeposit { .. } => {
                            metrics.transactions_stake_deposit += 1;
                        }
                        MaintenanceOutput::UpdateExchangeRate => {
                            metrics.transactions_update_exchange_rate += 1;
                        }
                        MaintenanceOutput::WithdrawInactiveStake { .. } => {
                            metrics.transactions_withdraw_inactive_stake += 1;
                        }
                        MaintenanceOutput::CollectValidatorFee { .. } => {
                            metrics.transactions_collect_validator_fee += 1
                        }
                        MaintenanceOutput::MergeStake { .. } => {
                            metrics.transactions_merge_stake += 1
                        }
                        MaintenanceOutput::ClaimValidatorFee { .. } => {
                            metrics.transactions_claim_validator_fee += 1
                        }
                        MaintenanceOutput::UnstakeFromInactiveValidator { .. } => {
                            metrics.transactions_unstake_from_inactive_validator += 1
                        }
                        MaintenanceOutput::RemoveValidator { .. } => {
                            metrics.transactions_remove_validator += 1
                        }
                    }
                }
            }

            Ok(state)
        });

        let state = match result {
            Ok(state) => Some(state),
            Err(err) => {
                println!("Error in maintenance.");
                err.print_pretty();
                metrics.errors += 1;

                // If the error was caused by a connectivity problem, we shouldn't
                // hammer the RPC again straight away. Even better would be to do
                // exponential backoff with jitter, but let's not go there right now.
                do_wait = true;

                None
            }
        };

        // Publish the new state and metrics, so the webserver can serve them.
        let snapshot = Snapshot {
            metrics: metrics.clone(),
            solido: state,
        };
        snapshot_mutex.lock().unwrap().replace(Arc::new(snapshot));

        if do_wait {
            // Sleep a random time, to avoid a thundering herd problem, in case
            // multiple maintainer bots happened to run in sync. They would all
            // try to create the same transaction, and only one would pass.
            let max_poll_interval = Duration::from_secs(*opts.max_poll_interval_seconds());
            let sleep_time = rng.gen_range(Duration::from_secs(0)..max_poll_interval);
            println!("Sleeping {:?} until next iteration ...", sleep_time);
            std::thread::sleep(sleep_time);
        }
    }
}

fn serve_request(request: Request, snapshot_mutex: &SnapshotMutex) -> Result<(), std::io::Error> {
    // Take the current snapshot. This only holds the lock briefly, and does
    // not prevent other threads from updating the snapshot while this request
    // handler is running.
    let option_snapshot = snapshot_mutex.lock().unwrap().clone();

    // It might be that no snapshot is available yet. This happens when we just
    // started the server, and the main loop has not yet queried the RPC for the
    // latest state.
    let snapshot = match option_snapshot {
        Some(arc_snapshot) => arc_snapshot,
        None => {
            return request.respond(
                Response::from_string(
                    "Service Unavailable\n\nServer is still starting, try again shortly.",
                )
                .with_status_code(503),
            );
        }
    };

    // We don't even look at the request, for now we always serve the metrics.

    let mut out: Vec<u8> = Vec::new();
    let mut is_ok = snapshot.metrics.write_prometheus(&mut out).is_ok();

    if let Some(ref solido) = snapshot.solido {
        is_ok = is_ok && solido.write_prometheus(&mut out).is_ok();
    }

    if is_ok {
        request.respond(Response::from_data(out))
    } else {
        request.respond(Response::from_string("error").with_status_code(500))
    }
}

/// Spawn threads that run the http server.
fn start_http_server(
    opts: &RunMaintainerOpts,
    snapshot_mutex: Arc<SnapshotMutex>,
) -> Vec<JoinHandle<()>> {
    let server = match Server::http(opts.listen().clone()) {
        Ok(server) => Arc::new(server),
        Err(err) => {
            eprintln!(
                "Error: {}\nFailed to start http server on {}. Is the daemon already running?",
                err,
                opts.listen(),
            );
            std::process::exit(1);
        }
    };

    println!("Http server listening on {}", opts.listen());

    // Spawn a number of http handler threads, so we can handle requests in
    // parallel. This server is only used to serve metrics, it can be super basic,
    // but some degree of parallelism is nice in case a client is slow to send
    // its request or something like that.
    (0..num_cpus::get())
        .map(|i| {
            let server_clone = server.clone();
            let snapshot_mutex_clone = snapshot_mutex.clone();
            std::thread::Builder::new()
                .name(format!("http_handler_{}", i))
                .spawn(move || {
                    for request in server_clone.incoming_requests() {
                        // Ignore any errors; if we fail to respond, then there's little
                        // we can do about it here ... the client should just retry.
                        let _ = serve_request(request, &*snapshot_mutex_clone);
                    }
                })
                .expect("Failed to spawn http handler thread.")
        })
        .collect()
}

/// Run the maintenance daemon.
pub fn main(config: &mut SnapshotClientConfig, opts: &RunMaintainerOpts) {
    let snapshot_mutex = Arc::new(Mutex::new(None));
    let http_threads = start_http_server(opts, snapshot_mutex.clone());

    run_main_loop(config, opts, &*snapshot_mutex);

    // We never get here, the main loop should run indefinitely until the program
    // is killed, and while the main loop runs, the http server also serves.
    for thread in http_threads {
        thread.join().unwrap();
    }
}
