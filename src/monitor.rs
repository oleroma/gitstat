//! The scheduler thread: watches the root, debounces events, runs checks on a
//! small worker pool and the periodic fetch sweep, and reports `Update`s.

use crate::config::Config;
use crate::git::{self, RepoStatus};
use crate::watch::{
    self, CONFIG_QUIET, Class, Debouncer, Need, REPOSET_MAX_WAIT, REPOSET_QUIET, RepoRoute, Router,
};
use notify::{EventKindMask, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

/// Concurrent `git` checks, as in `git::check_all`.
const MAX_WORKERS: usize = 6;
const HEALTH_EVERY: Duration = Duration::from_secs(60);
/// Without a working watch: a local sweep this often, and a new watch attempt.
const POLL_EVERY: Duration = Duration::from_secs(60);
const RETRY_WATCH_EVERY: Duration = Duration::from_secs(5 * 60);
/// A wall-clock jump this much larger than the monotonic one means the
/// machine slept.
const RESUME_JUMP: Duration = Duration::from_secs(30);
/// After resume the network needs a moment before a fetch can work.
const RESUME_FETCH_DELAY: Duration = Duration::from_secs(15);
/// Retry delays for a repo whose fetch failed, then the last one repeats.
const FETCH_BACKOFF: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(2 * 60),
    Duration::from_secs(5 * 60),
];
const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);
/// A rediscovery the user didn't ask for (folder events, a rescan, a local
/// sweep) starts no sooner than this many walk durations after the last
/// walk ended, so folder churn on a big root (a whole drive) can't keep the
/// disk busy walking.
const WALK_GAP_FACTOR: u32 = 10;
/// A walk taking longer than this is logged.
const SLOW_WALK: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchMode {
    Events,
    Polling {
        reason: String,
    },
    /// `watch: false`: interval sweeps only.
    Off,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    RefreshAll,
    /// A config the app changed in memory, e.g. a menu toggle that
    /// config.json (unparsable mid-edit) couldn't take. A later good reload
    /// of the file replaces it, as it does in the app.
    Apply(Config),
    Shutdown,
}

#[derive(Clone, Debug)]
pub enum Update {
    Config(Result<Config, String>),
    Repos {
        repos: Vec<PathBuf>,
        /// Why the list may be incomplete (see `Discovery::capped`).
        capped: Option<String>,
    },
    Started {
        repo: PathBuf,
        // The app shows every check alike; the tests tell fetches apart.
        #[cfg_attr(not(test), allow(dead_code))]
        fetch: bool,
    },
    Status(RepoStatus),
    SweepStarted,
    SweepDone,
    Mode(WatchMode),
}

enum Msg {
    Cmd(Command),
    Fs(notify::Result<notify::Event>),
    Done(Done),
    Walked(Walked),
}

/// A repo walk's result, from the walk thread.
struct Walked {
    /// The root generation the walk was started for.
    generation: u64,
    /// Which walk this was (see `Scheduler::walks`).
    seq: u64,
    /// `None` if the walk panicked.
    discovery: Option<git::Discovery>,
    took: Duration,
}

struct Done {
    /// The root generation the check was started for.
    generation: u64,
    repo: PathBuf,
    need: Need,
    status: RepoStatus,
    ignored: Vec<String>,
}

pub struct Monitor {
    tx: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    pub fn start(config_file: PathBuf, sink: impl Fn(Update) + Send + 'static) -> Monitor {
        let (tx, rx) = mpsc::channel();
        // Event paths come back in the form the watch was given; make it absolute
        // so they compare equal to the config file.
        let config_file = std::path::absolute(&config_file).unwrap_or(config_file);
        let scheduler_tx = tx.clone();
        let thread = thread::Builder::new()
            .name("gitstat-monitor".into())
            .spawn(move || Scheduler::new(config_file, Box::new(sink), scheduler_tx).run(rx))
            .ok();
        Monitor { tx, thread }
    }

    pub fn send(&self, c: Command) {
        let _ = self.tx.send(Msg::Cmd(c));
    }
}

impl Drop for Monitor {
    /// Waits briefly for the scheduler; running `git` workers are left to finish.
    fn drop(&mut self) {
        self.send(Command::Shutdown);
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        while !thread.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
    }
}

struct Retry {
    failures: usize,
    at: Option<Instant>,
}

struct Scheduler {
    config_file: PathBuf,
    sink: Box<dyn Fn(Update) + Send>,
    tx: mpsc::Sender<Msg>,
    config: Config,
    /// The last reload result, so the sweep-time reload reports only changes.
    loaded: Result<Config, String>,
    root: PathBuf,
    /// `config.depth()` as of the last re-root.
    depth: usize,
    /// Bumped when the root or depth changes; results for an older root are
    /// dropped.
    generation: u64,
    watcher: Option<RecommendedWatcher>,
    /// Git dirs outside the root that need their own watch.
    external: Vec<PathBuf>,
    mode: WatchMode,
    router: Router,
    debouncer: Debouncer,
    repos: Vec<PathBuf>,
    /// The last walk's cap message, if a cap cut it short.
    capped: Option<String>,
    /// When the last repo walk ended and how long it took.
    walked: Option<(Instant, Duration)>,
    /// Walks started on the walk thread so far; each one's number.
    walks: u64,
    /// The walk running on the walk thread, if one is.
    walking: Option<u64>,
    /// Another walk was asked for while one ran: it starts when that ends.
    walk_again: bool,
    /// The running sweep's fetches wait for this walk (or a later one), so
    /// the repos it finds are fetched too.
    sweep_walk: Option<u64>,
    /// The last status sent per repo, for `keep_fetch_error`.
    statuses: HashMap<PathBuf, RepoStatus>,
    /// Linked worktrees whose git dirs couldn't be read; retried on every
    /// rediscovery until they can.
    unresolved: HashSet<PathBuf>,
    running: usize,
    /// Repos the current fetch sweep still waits for.
    sweep: HashSet<PathBuf>,
    sweeping: bool,
    sweeps: u64,
    /// `None` while a sweep runs: the next one is timed from its end.
    next_sweep: Option<Instant>,
    retries: HashMap<PathBuf, Retry>,
    next_health: Option<Instant>,
    poll_at: Option<Instant>,
    retry_watch_at: Option<Instant>,
    reposet_at: Option<Instant>,
    /// When the first event (or rescan) asking for the pending walk came,
    /// which bounds how far later ones can push it back.
    reposet_first: Option<Instant>,
    config_at: Option<Instant>,
    last_wall: SystemTime,
    last_mono: Instant,
    /// The deadline the loop last waited for, to notice oversleeping.
    waited_until: Option<Instant>,
}

impl Scheduler {
    fn new(config_file: PathBuf, sink: Box<dyn Fn(Update) + Send>, tx: mpsc::Sender<Msg>) -> Self {
        let loaded = Config::load_or_create(&config_file);
        let config = loaded.clone().unwrap_or_default();
        let root = config.repos_dir();
        let depth = config.depth();
        Self {
            router: Router::new(root.clone(), depth, config_file.clone()),
            config_file,
            sink,
            tx,
            config,
            loaded,
            root,
            depth,
            generation: 0,
            watcher: None,
            external: Vec::new(),
            mode: WatchMode::Off,
            debouncer: Debouncer::default(),
            repos: Vec::new(),
            capped: None,
            walked: None,
            walks: 0,
            walking: None,
            walk_again: false,
            sweep_walk: None,
            statuses: HashMap::new(),
            unresolved: HashSet::new(),
            running: 0,
            sweep: HashSet::new(),
            sweeping: false,
            sweeps: 0,
            next_sweep: None,
            retries: HashMap::new(),
            next_health: None,
            poll_at: None,
            retry_watch_at: None,
            reposet_at: None,
            reposet_first: None,
            config_at: None,
            last_wall: SystemTime::now(),
            last_mono: Instant::now(),
            waited_until: None,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Msg>) {
        let now = Instant::now();
        (self.sink)(Update::Config(self.loaded.clone()));
        // Here, not on the walk thread: there is nothing to do without it.
        self.walk_here(true, now);
        self.setup_watch(now);
        // `set_mode` reports only changes, and the mode starts out as Off.
        if self.mode == WatchMode::Off {
            self.emit(Update::Mode(WatchMode::Off));
        }
        self.start_sweep(false, now);

        loop {
            let now = Instant::now();
            self.detect_resume(now);
            self.run_timers(now);
            self.dispatch(now);
            let deadline = self.next_deadline();
            self.waited_until = deadline;
            let msg = match deadline {
                Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match msg {
                Ok(Msg::Cmd(Command::Shutdown)) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Msg::Cmd(Command::RefreshAll)) => {
                    let now = Instant::now();
                    self.poll_config(now);
                    self.start_sweep(true, now);
                }
                Ok(Msg::Cmd(Command::Apply(config))) => {
                    if config != self.config {
                        self.apply_config(config, Instant::now());
                    }
                }
                Ok(Msg::Fs(event)) => self.on_fs(event, Instant::now()),
                Ok(Msg::Done(done)) => self.on_done(done, Instant::now()),
                Ok(Msg::Walked(walked)) => self.on_walked(walked, Instant::now()),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn emit(&self, update: Update) {
        (self.sink)(update);
    }

    fn set_mode(&mut self, mode: WatchMode) {
        if self.mode != mode {
            self.mode = mode.clone();
            self.emit(Update::Mode(mode));
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        // With every worker busy, the next `Done` is what frees a slot.
        let debounce = (self.running < MAX_WORKERS)
            .then(|| self.debouncer.next_deadline())
            .flatten();
        [
            self.next_sweep,
            self.next_health,
            self.poll_at,
            self.retry_watch_at,
            self.reposet_at.filter(|_| self.walking.is_none()),
            self.config_at,
            debounce,
            self.retries.values().filter_map(|r| r.at).min(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Sleep shows up as the wall clock running ahead of the monotonic one
    /// (Linux, macOS), or as a wait that ended far past its deadline
    /// (Windows, where `Instant` keeps counting through sleep).
    fn detect_resume(&mut self, now: Instant) {
        let wall = SystemTime::now();
        let mono = now.saturating_duration_since(self.last_mono);
        let wall_delta = wall.duration_since(self.last_wall).unwrap_or_default();
        let late = self
            .waited_until
            .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        self.last_wall = wall;
        self.last_mono = now;
        self.waited_until = None;
        if wall_delta > mono + RESUME_JUMP || late > RESUME_JUMP {
            self.local_sweep(now);
            self.next_sweep = Some(now + RESUME_FETCH_DELAY);
            if self.mode == WatchMode::Events {
                self.next_health = Some(now);
            }
        }
    }

    fn run_timers(&mut self, now: Instant) {
        let due = |at: Option<Instant>| at.is_some_and(|at| at <= now);
        if due(self.config_at) {
            self.config_at = None;
            self.reload_config(now);
        }
        // While a walk runs this waits for it, to keep the walk gap.
        if self.walking.is_none() && due(self.reposet_at) {
            self.spawn_walk();
        }
        if due(self.next_sweep) {
            self.poll_config(now);
            self.start_sweep(true, now);
        }
        if due(self.next_health) {
            self.next_health = Some(now + HEALTH_EVERY);
            self.health_check(now);
        }
        if due(self.poll_at) {
            self.poll_at = Some(now + POLL_EVERY);
            self.local_sweep(now);
        }
        if due(self.retry_watch_at) {
            self.retry_watch_at = Some(now + RETRY_WATCH_EVERY);
            self.setup_watch(now);
            if self.mode == WatchMode::Events {
                self.local_sweep(now);
            }
        }
        let imminent = self
            .next_sweep
            .is_some_and(|at| at <= now + FETCH_BACKOFF[0]);
        for (repo, retry) in &mut self.retries {
            if retry.at.is_some_and(|at| at <= now) {
                retry.at = None;
                // The sweep is about to fetch it anyway.
                if !imminent {
                    self.debouncer.request(repo, Need::Fetch, now);
                }
            }
        }
    }

    fn dispatch(&mut self, now: Instant) {
        let free = MAX_WORKERS.saturating_sub(self.running);
        let due = self.debouncer.due(now, free);
        // A deferred rescan just ran; it may have missed new repos too.
        if self.debouncer.take_rescanned() {
            self.schedule_walk(now);
        }
        for (repo, need) in due {
            let fetch = need == Need::Fetch;
            let tx = self.tx.clone();
            let generation = self.generation;
            let worker_repo = repo.clone();
            let root = self.root.clone();
            let spawned = thread::Builder::new()
                .name("gitstat-check".into())
                .spawn(move || {
                    // Always report back, even if the check panics, so the repo
                    // never stays in flight.
                    let (status, ignored) = std::panic::catch_unwind(|| {
                        git::check_repo_detailed(&root, &worker_repo, fetch)
                    })
                    .unwrap_or_else(|_| {
                        let status = git::failed(&root, &worker_repo, "internal error");
                        (status, Vec::new())
                    });
                    let _ = tx.send(Msg::Done(Done {
                        generation,
                        repo: worker_repo,
                        need,
                        status,
                        ignored,
                    }));
                });
            self.emit(Update::Started {
                repo: repo.clone(),
                fetch,
            });
            self.running += 1;
            if let Err(e) = spawned {
                self.on_done(
                    Done {
                        generation,
                        status: git::failed(&self.root, &repo, &e.to_string()),
                        repo,
                        need,
                        ignored: Vec::new(),
                    },
                    now,
                );
            }
        }
    }

    fn on_done(&mut self, done: Done, now: Instant) {
        let Done {
            generation,
            repo,
            need,
            status,
            ignored,
        } = done;
        self.running = self.running.saturating_sub(1);
        // A check for an older root still held its repo; release it, then
        // drop the result.
        self.debouncer.finished(&repo, now);
        if generation != self.generation {
            return;
        }
        if !self.repos.contains(&repo) {
            return;
        }
        if status.error.is_none() || status.fetch_failed {
            self.router.set_ignored(&repo, ignored);
        }
        let status = match (need, self.statuses.get(&repo)) {
            (Need::Local, Some(prev)) => git::keep_fetch_error(prev, status),
            _ => status,
        };
        if need == Need::Fetch {
            // Even when the local status failed too: the fetch still did.
            if status.fetch_errored {
                let retry = self.retries.entry(repo.clone()).or_insert(Retry {
                    failures: 0,
                    at: None,
                });
                let delay = FETCH_BACKOFF[retry.failures.min(FETCH_BACKOFF.len() - 1)];
                retry.failures += 1;
                retry.at = Some(now + delay);
            } else {
                self.retries.remove(&repo);
            }
        }
        self.statuses.insert(repo.clone(), status.clone());
        self.emit(Update::Status(status));
        if need == Need::Fetch && self.sweep.remove(&repo) {
            self.finish_sweep_if_done(now);
        }
    }

    /// A fetch of every repo. Every sweep looks for new repos first (`walk`,
    /// unless a walk just ran): events can be dropped silently (network
    /// shares), and `watch: false` has none. The fetches start once that
    /// walk is in, so the repos it finds are fetched too.
    fn start_sweep(&mut self, walk: bool, now: Instant) {
        if !self.sweeping {
            self.sweeping = true;
            self.emit(Update::SweepStarted);
        }
        self.next_sweep = None;
        self.sweeps += 1;
        // The sweep replaces any pending retry.
        for retry in self.retries.values_mut() {
            retry.at = None;
        }
        if walk {
            // The walk started now, or the one queued behind a running one.
            self.sweep_walk = Some(self.walks + 1);
            self.request_walk();
        } else {
            self.sweep_walk = None;
            self.fetch_all(now);
        }
    }

    fn fetch_all(&mut self, now: Instant) {
        for repo in &self.repos {
            self.sweep.insert(repo.clone());
            self.debouncer.request(repo, Need::Fetch, now);
        }
        self.finish_sweep_if_done(now);
    }

    fn finish_sweep_if_done(&mut self, now: Instant) {
        if self.sweeping && self.sweep_walk.is_none() && self.sweep.is_empty() {
            self.sweeping = false;
            let interval = jittered(self.config.interval(), &self.root, self.sweeps);
            self.next_sweep = Some(now + interval);
            self.emit(Update::SweepDone);
        }
    }

    /// A Local check of every repo, and a fresh look for new ones. Events
    /// may have been missed (resume, no watch), so running checks rerun.
    fn local_sweep(&mut self, now: Instant) {
        self.schedule_walk(now);
        for repo in &self.repos {
            self.debouncer.recheck(repo, now);
        }
    }

    /// Lost events: a recheck of every repo and a fresh walk, rate-limited
    /// together. A deferred rescan rediscovers when it runs, in `dispatch`.
    fn rescan(&mut self, now: Instant) {
        if self.debouncer.rescan(now) {
            self.schedule_walk(now);
        }
    }

    fn on_fs(&mut self, event: notify::Result<notify::Event>, now: Instant) {
        let event = match event {
            Ok(event) => event,
            Err(_) => {
                // Lost events, and maybe a dead watch: the health check tells.
                if self.mode == WatchMode::Events {
                    self.rescan(now);
                    self.next_health = Some(now);
                }
                return;
            }
        };
        let events_on = self.mode == WatchMode::Events;
        if event.need_rescan() && events_on {
            self.rescan(now);
        }
        for path in &event.paths {
            // The root itself renamed or deleted: its watch is likely gone.
            if events_on && watch::same_path(path, &self.root) {
                self.next_health = Some(now);
            }
            for (repo, class) in self.router.classify(path, &event.kind) {
                match (class, repo) {
                    (Class::Config, _) => self.config_at = Some(now + CONFIG_QUIET),
                    // Only the config file's own watch is live outside Events mode.
                    _ if !events_on => {}
                    (Class::RepoSet, _) => self.schedule_walk(now),
                    (Class::Ignore, _) | (_, None) => {}
                    (class, Some(repo)) => self.debouncer.on_event(&repo, class, now),
                }
            }
        }
    }

    /// Asks for a walk the user didn't: after a trailing quiet window, at
    /// least `WALK_GAP_FACTOR` times the last walk's duration after it
    /// ended, and no later than `REPOSET_MAX_WAIT` after the first request
    /// (or that gap), however steady the churn.
    fn schedule_walk(&mut self, now: Instant) {
        let gap = self.walked.map(|(end, took)| end + took * WALK_GAP_FACTOR);
        let first = *self.reposet_first.get_or_insert(now);
        let quiet = (now + REPOSET_QUIET).max(gap.unwrap_or(now));
        let latest = first.max(gap.unwrap_or(first)) + REPOSET_MAX_WAIT;
        self.reposet_at = Some(quiet.min(latest));
    }

    /// Starts a walk on the walk thread, or queues one behind the walk
    /// running there.
    fn request_walk(&mut self) {
        if self.walking.is_some() {
            self.walk_again = true;
        } else {
            self.spawn_walk();
        }
    }

    /// The walk covers whatever asked for one so far.
    fn spawn_walk(&mut self) {
        self.walks += 1;
        self.walk_again = false;
        self.reposet_at = None;
        self.reposet_first = None;
        let (seq, generation) = (self.walks, self.generation);
        let (root, depth, tx) = (self.root.clone(), self.depth, self.tx.clone());
        let spawned = thread::Builder::new()
            .name("gitstat-walk".into())
            .spawn(move || {
                let started = Instant::now();
                let discovery = std::panic::catch_unwind(|| git::find_repos(&root, depth)).ok();
                let _ = tx.send(Msg::Walked(Walked {
                    generation,
                    seq,
                    discovery,
                    took: started.elapsed(),
                }));
            });
        self.walking = Some(seq);
        if spawned.is_err() {
            // No thread to spare: walk here instead.
            let started = Instant::now();
            let discovery = Some(git::find_repos(&self.root, self.depth));
            let walked = Walked {
                generation,
                seq,
                discovery,
                took: started.elapsed(),
            };
            self.on_walked(walked, Instant::now());
        }
    }

    fn on_walked(&mut self, walked: Walked, now: Instant) {
        let Walked {
            generation,
            seq,
            discovery,
            took,
        } = walked;
        if self.walking == Some(seq) {
            self.walking = None;
        }
        self.walk_took(took, now);
        // A walk for an older root is dropped; the new root walked already.
        if generation == self.generation
            && let Some(discovery) = discovery
        {
            self.apply_discovery(discovery, false, now);
        }
        if self.walking.is_none() {
            if self.walk_again {
                self.spawn_walk();
            } else if self.reposet_at.is_some() {
                // Asked for while this one ran: again, after the gap.
                self.schedule_walk(now);
            }
        }
        if self.sweep_walk.is_some_and(|wanted| seq >= wanted) {
            self.sweep_walk = None;
            self.fetch_all(now);
        }
    }

    fn walk_took(&mut self, took: Duration, now: Instant) {
        self.walked = Some((now, took));
        if took > SLOW_WALK {
            eprintln!(
                "gitstat: looking for repos in {} took {took:.1?}",
                self.root.display()
            );
        }
    }

    /// Walks on this thread, for a start or a new root, where nothing can
    /// go on without the repo list. `force` reports it even unchanged.
    fn walk_here(&mut self, force: bool, now: Instant) {
        let started = Instant::now();
        let discovery = git::find_repos(&self.root, self.depth);
        self.reposet_first = None;
        self.reposet_at = None;
        let end = Instant::now();
        self.walk_took(end - started, end);
        self.apply_discovery(discovery, force, now);
    }

    /// The sweep-time reload: a fallback for a missed config event, so it
    /// reports only a change.
    fn poll_config(&mut self, now: Instant) {
        if let Some(loaded) = read_config(&self.config_file)
            && loaded != self.loaded
        {
            self.apply_loaded(loaded, now);
        }
    }

    fn reload_config(&mut self, now: Instant) {
        if let Some(loaded) = read_config(&self.config_file) {
            self.apply_loaded(loaded, now);
        }
    }

    fn apply_loaded(&mut self, loaded: Result<Config, String>, now: Instant) {
        self.loaded = loaded.clone();
        self.emit(Update::Config(loaded.clone()));
        // A broken file keeps the last good config running.
        if let Ok(config) = loaded
            && config != self.config
        {
            self.apply_config(config, now);
        }
    }

    fn apply_config(&mut self, config: Config, now: Instant) {
        let old = std::mem::replace(&mut self.config, config);
        let root = self.config.repos_dir();
        let depth = self.config.depth();
        if !same_root(&root, &self.root) || depth != self.depth {
            self.generation += 1;
            self.root = root;
            self.depth = depth;
            self.router = Router::new(self.root.clone(), depth, self.config_file.clone());
            // Checks still running for the old root keep their repos busy.
            self.debouncer.next_root();
            self.repos.clear();
            self.capped = None;
            self.statuses.clear();
            self.unresolved.clear();
            self.retries.clear();
            self.sweep.clear();
            self.sweep_walk = None;
            self.external.clear();
            self.watcher = None;
            self.walk_here(true, now);
            self.setup_watch(now);
            self.start_sweep(false, now);
            return;
        }
        if old.watch != self.config.watch {
            self.setup_watch(now);
            if self.mode == WatchMode::Events {
                // Changes made while not watching.
                self.local_sweep(now);
            }
        }
        if old.interval() != self.config.interval() && self.next_sweep.is_some() {
            self.next_sweep = Some(now + jittered(self.config.interval(), &self.root, self.sweeps));
        }
    }

    /// Takes in a walk's repo list and rebuilds the routes. `force` reports
    /// it even unchanged. Routes are rebuilt when the list changed or a
    /// worktree's git dirs couldn't be read last time.
    fn apply_discovery(&mut self, discovery: git::Discovery, force: bool, now: Instant) {
        let git::Discovery {
            repos: found,
            capped,
            pruned,
            entered,
        } = discovery;
        self.router.set_pruned(&pruned);
        self.router.set_entered(&entered);
        self.router.set_capped(capped.is_some());
        let changed = found != self.repos || capped != self.capped;
        self.capped = capped;
        if !changed && !force && self.unresolved.is_empty() {
            return;
        }
        let added: Vec<PathBuf> = found
            .iter()
            .filter(|r| !self.repos.contains(r))
            .cloned()
            .collect();
        self.repos = found;
        let repos = &self.repos;
        self.debouncer.retain(repos);
        self.statuses.retain(|path, _| repos.contains(path));
        self.retries.retain(|path, _| repos.contains(path));
        self.sweep.retain(|path| repos.contains(path));

        let ignored: HashMap<PathBuf, Vec<String>> = self
            .router
            .routes()
            .map(|r| (r.path.clone(), r.ignored.clone()))
            .collect();
        self.unresolved.clear();
        let mut routes: Vec<RepoRoute> = Vec::new();
        for repo in &self.repos {
            let (mut route, resolved) = route_for(repo, &self.root);
            route.ignored = ignored.get(repo).cloned().unwrap_or_default();
            if !resolved {
                self.unresolved.insert(repo.clone());
            }
            routes.push(route);
        }
        self.update_external(&routes);
        self.debouncer.set_groups(&routes);
        self.router.set_repos(routes);

        // The startup sweep or a root change covers every repo already.
        if !force {
            for repo in &added {
                self.debouncer.request(repo, Need::Local, now);
            }
        }
        if changed || force {
            self.emit(Update::Repos {
                repos: self.repos.clone(),
                capped: self.capped.clone(),
            });
        }
        self.finish_sweep_if_done(now);
    }

    /// Watches git dirs that live outside the root (linked worktrees of a
    /// repo elsewhere).
    fn update_external(&mut self, routes: &[RepoRoute]) {
        let mut dirs: Vec<PathBuf> = Vec::new();
        for dir in routes.iter().flat_map(|r| [&r.git_dir, &r.common_dir]) {
            if !watch::is_within(dir, &self.root) && !dirs.iter().any(|d| watch::same_path(d, dir))
            {
                dirs.push(dir.clone());
            }
        }
        // A common dir covers the worktree dirs inside it.
        let all = dirs.clone();
        dirs.retain(|d| {
            !all.iter()
                .any(|other| !watch::same_path(other, d) && watch::is_within(d, other))
        });
        if let Some(watcher) = &mut self.watcher
            && self.mode == WatchMode::Events
        {
            for gone in self.external.iter().filter(|d| !dirs.contains(d)) {
                let _ = watcher.unwatch(gone);
            }
            for new in dirs.iter().filter(|d| !self.external.contains(d)) {
                let _ = watcher.watch(new, RecursiveMode::Recursive);
            }
        }
        self.external = dirs;
    }

    /// (Re)creates the watcher: the root recursively when watching is on,
    /// the config file's folder when the root doesn't cover it, and the
    /// external git dirs.
    fn setup_watch(&mut self, now: Instant) {
        self.watcher = None;
        let tx = self.tx.clone();
        let handler = move |event| {
            let _ = tx.send(Msg::Fs(event));
        };
        let options = notify::Config::default().with_event_kinds(EventKindMask::CORE);
        let mut watcher = match RecommendedWatcher::new(handler, options) {
            Ok(watcher) => watcher,
            Err(e) => {
                self.enter_polling(format!("can't watch: {e}"), now);
                return;
            }
        };
        let root_result = if self.config.watch {
            Some(watcher.watch(&self.root, RecursiveMode::Recursive))
        } else {
            None
        };
        let root_ok = matches!(root_result, Some(Ok(())));
        let config_dir = match self.config_file.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => PathBuf::from("."),
        };
        if !(root_ok && watch::is_within(&config_dir, &self.root)) {
            // Best effort: the sweep-time reload still catches edits.
            let _ = watcher.watch(&config_dir, RecursiveMode::NonRecursive);
        }
        if root_ok {
            for dir in &self.external {
                let _ = watcher.watch(dir, RecursiveMode::Recursive);
            }
        }
        self.watcher = Some(watcher);
        match root_result {
            None => {
                self.poll_at = None;
                self.retry_watch_at = None;
                self.next_health = None;
                self.set_mode(WatchMode::Off);
            }
            Some(Ok(())) => {
                self.poll_at = None;
                self.retry_watch_at = None;
                self.next_health = Some(now + HEALTH_EVERY);
                self.set_mode(WatchMode::Events);
            }
            Some(Err(e)) => self.enter_polling(format!("{}: {e}", self.root.display()), now),
        }
    }

    fn enter_polling(&mut self, reason: String, now: Instant) {
        if !self.config.watch {
            self.set_mode(WatchMode::Off);
            return;
        }
        self.next_health = None;
        self.poll_at.get_or_insert(now + POLL_EVERY);
        self.retry_watch_at.get_or_insert(now + RETRY_WATCH_EVERY);
        self.set_mode(WatchMode::Polling { reason });
    }

    /// A watch can die quietly (a network share dropping). Re-watch, and
    /// check everything if events may have been missed.
    fn health_check(&mut self, now: Instant) {
        if self.mode != WatchMode::Events {
            return;
        }
        let alive = self.watcher.as_ref().is_some_and(|w| {
            w.watched_paths().map_or(true, |paths| {
                paths.iter().any(|(p, _)| watch::same_path(p, &self.root))
            })
        });
        if !alive {
            // Events may have been lost whether or not the new watch works.
            self.setup_watch(now);
            self.rescan(now);
        }
    }
}

/// Reads the config file for a reload. A missing file (mid-save, or
/// deleted) is not reported: re-creating it here would fight the editor.
fn read_config(file: &Path) -> Option<Result<Config, String>> {
    if !file.exists() {
        return None;
    }
    Some(Config::load_or_create(file))
}

/// The routing entry for a repo, and whether its git dirs are known. A
/// `.git` folder needs no git call; a `.git` file (linked worktree) asks git
/// where its dirs are, mapped back under the root's spelling when they are
/// inside it. If git can't say, the route falls back to `.git` (false).
fn route_for(repo: &Path, root: &Path) -> (RepoRoute, bool) {
    let dot_git = repo.join(".git");
    let (git_dir, common_dir, resolved) = if dot_git.is_dir() {
        (dot_git.clone(), dot_git, true)
    } else {
        match git::git_dirs(repo) {
            Some((git_dir, common)) => (respell(&git_dir, root), respell(&common, root), true),
            None => (dot_git.clone(), dot_git, false),
        }
    };
    let route = RepoRoute {
        path: repo.to_path_buf(),
        git_dir,
        common_dir,
        ignored: Vec::new(),
    };
    (route, resolved)
}

/// `D:/dev` and `D:\dev` are the same root, and so are two spellings of
/// one existing folder (`D:\dev\.`, a junction, a subst drive).
fn same_root(a: &Path, b: &Path) -> bool {
    if watch::same_path(a, b) {
        return true;
    }
    matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// git reports resolved paths; events use the root as configured (maybe
/// through a symlink, or an 8.3 name on Windows).
fn respell(path: &Path, root: &Path) -> PathBuf {
    let (Ok(real_path), Ok(real_root)) = (path.canonicalize(), root.canonicalize()) else {
        return path.to_path_buf();
    };
    match real_path.strip_prefix(&real_root) {
        Ok(rel) => root.join(rel),
        Err(_) => path.to_path_buf(),
    }
}

/// `interval` give or take up to 10%, so many gitstats (or many sweeps)
/// don't hit a shared remote in lockstep. Derived from the root and the
/// sweep count: no randomness crate needed.
fn jittered(interval: Duration, root: &Path, sweep: u64) -> Duration {
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    sweep.hash(&mut hasher);
    let unit = (hasher.finish() % 2001) as f64 / 1000.0 - 1.0;
    interval.mul_f64(1.0 + 0.1 * unit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Process;
    use std::sync::mpsc::Receiver;

    #[test]
    fn jitter_stays_within_ten_percent() {
        let interval = Duration::from_secs(300);
        let root = Path::new("/dev");
        let values: Vec<Duration> = (0..200).map(|n| jittered(interval, root, n)).collect();
        assert!(values.iter().all(|d| *d >= Duration::from_secs(270)));
        assert!(values.iter().all(|d| *d <= Duration::from_secs(330)));
        let distinct: HashSet<_> = values.iter().collect();
        assert!(distinct.len() > 100, "jitter varies between sweeps");
        assert_eq!(jittered(interval, root, 7), jittered(interval, root, 7));
        assert_ne!(
            jittered(interval, root, 7),
            jittered(interval, Path::new("/other"), 7)
        );
    }

    #[test]
    fn respell_keeps_paths_outside_the_root() {
        let dir = temp("respell");
        let root = dir.join("root");
        std::fs::create_dir_all(root.join("r/.git")).unwrap();
        let real = root.canonicalize().unwrap().join("r/.git");
        assert_eq!(respell(&real, &root), root.join("r/.git"));
        assert_eq!(respell(&dir, &root), dir);
        let missing = dir.join("missing");
        assert_eq!(respell(&missing, &root), missing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_worktree_routes_are_marked() {
        let dir = temp("route");
        init_repo(&dir.join("r"));
        let wt = dir.join("wt");
        git(
            &dir.join("r"),
            &["worktree", "add", "-q", wt.to_str().unwrap()],
        );
        let (route, resolved) = route_for(&wt, &dir);
        assert!(resolved);
        assert_ne!(route.common_dir, wt.join(".git"));
        // Its metadata gone (pruned, or not written yet): a fallback route,
        // which `rediscover` retries.
        std::fs::write(wt.join(".git"), "gitdir: /nonexistent/gitstat\n").unwrap();
        let (route, resolved) = route_for(&wt, &dir);
        assert!(!resolved);
        assert_eq!(route.git_dir, wt.join(".git"));
        assert!(route_for(&dir.join("r"), &dir).1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_root_spellings() {
        let dir = temp("same-root");
        std::fs::create_dir_all(dir.join("x")).unwrap();
        assert!(same_root(&dir, &dir.join(".")));
        assert!(same_root(&dir.join("x/.."), &dir));
        assert!(!same_root(&dir.join("x"), &dir));
        assert!(!same_root(&dir, &dir.join("missing")));
        assert!(same_root(Path::new("/no/such/a/"), Path::new("/no/such/a")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Integration tests: a real watcher (inotify here, ReadDirectoryChangesW
    // on Windows CI) on a temp root, driven through `Monitor`.

    const WAIT: Duration = Duration::from_secs(5);

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gitstat-mon-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Process::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "init.defaultBranch=main", "-c", "core.autocrlf=false"])
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repo with one commit.
    fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q"]);
        std::fs::write(path.join("README"), "hi\n").unwrap();
        git(path, &["add", "README"]);
        git(path, &["commit", "-q", "-m", "init"]);
    }

    struct Harness {
        dir: PathBuf,
        root: PathBuf,
        config_file: PathBuf,
        rx: Receiver<Update>,
        monitor: Option<Monitor>,
    }

    impl Harness {
        /// `dir/root` holds the repos; the config lives outside it in `dir/app`.
        fn new(name: &str, setup: impl FnOnce(&Path), config: impl FnOnce(&mut Config)) -> Self {
            let dir = temp(name);
            let root = dir.join("root");
            std::fs::create_dir_all(&root).unwrap();
            setup(&root);
            let app = dir.join("app");
            std::fs::create_dir_all(&app).unwrap();
            let config_file = app.join("config.json");
            let mut cfg = Config {
                path: root.to_string_lossy().into_owned(),
                ..Config::default()
            };
            config(&mut cfg);
            write_config(&config_file, &cfg);
            let (tx, rx) = mpsc::channel();
            let monitor = Monitor::start(config_file.clone(), move |u| {
                let _ = tx.send(u);
            });
            Self {
                dir,
                root,
                config_file,
                rx,
                monitor: Some(monitor),
            }
        }

        fn send(&self, c: Command) {
            if let Some(monitor) = &self.monitor {
                monitor.send(c);
            }
        }

        /// The first update matching `pred` within `timeout`.
        fn wait_for(&self, timeout: Duration, pred: impl Fn(&Update) -> bool) -> Option<Update> {
            let deadline = Instant::now() + timeout;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.rx.recv_timeout(left) {
                    Ok(u) if pred(&u) => return Some(u),
                    Ok(_) => {}
                    Err(_) => return None,
                }
            }
        }

        /// Every update over the next `window`.
        fn collect(&self, window: Duration) -> Vec<Update> {
            let deadline = Instant::now() + window;
            let mut out = Vec::new();
            while let Ok(u) = self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                out.push(u);
            }
            out
        }

        /// Every update up to and including the next `SweepDone`.
        fn until_sweep_done(&self) -> Vec<Update> {
            let deadline = Instant::now() + WAIT;
            let mut out = Vec::new();
            while let Ok(u) = self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                let done = matches!(u, Update::SweepDone);
                out.push(u);
                if done {
                    return out;
                }
            }
            panic!("no SweepDone within {WAIT:?}: {out:?}");
        }

        /// Waits for the startup sweep to finish.
        fn settle(&self) {
            assert!(
                self.wait_for(WAIT, |u| matches!(u, Update::SweepDone))
                    .is_some(),
                "startup sweep never finished"
            );
        }

        fn status(&self, repo: &str, timeout: Duration) -> Option<RepoStatus> {
            let path = self.root.join(repo);
            match self.wait_for(
                timeout,
                |u| matches!(u, Update::Status(s) if s.path == path),
            ) {
                Some(Update::Status(s)) => Some(s),
                _ => None,
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.monitor.take();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn write_config(file: &Path, config: &Config) {
        let text = serde_json::to_string_pretty(config).unwrap();
        std::fs::write(file, text).unwrap();
    }

    fn started(updates: &[Update]) -> Vec<&Update> {
        updates
            .iter()
            .filter(|u| matches!(u, Update::Started { .. }))
            .collect()
    }

    #[test]
    fn startup_reports_config_repos_mode_and_statuses() {
        let h = Harness::new("startup", |root| init_repo(&root.join("a")), |_| {});
        let a = h.root.join("a");
        let updates = h.until_sweep_done();
        assert!(matches!(&updates[0], Update::Config(Ok(c)) if c.repos_dir() == h.root));
        assert!(matches!(
            &updates[1],
            Update::Repos { repos: r, capped: None } if *r == vec![a.clone()]
        ));
        assert!(matches!(&updates[2], Update::Mode(WatchMode::Events)));
        assert!(matches!(&updates[3], Update::SweepStarted));
        assert!(
            updates
                .iter()
                .any(|u| matches!(u, Update::Started { repo, fetch: true } if *repo == a))
        );
        assert!(updates.iter().any(|u| matches!(
            u,
            Update::Status(s) if s.path == a && s.error.is_none() && !s.dirty
        )));
    }

    #[test]
    fn edit_reports_dirty() {
        let h = Harness::new("edit", |root| init_repo(&root.join("a")), |_| {});
        h.settle();
        std::fs::write(h.root.join("a").join("README"), "changed\n").unwrap();
        let status = h.status("a", WAIT).expect("no status after an edit");
        assert!(status.dirty, "{status:?}");
    }

    #[test]
    fn commit_reports_clean() {
        let h = Harness::new(
            "commit",
            |root| {
                init_repo(&root.join("a"));
                std::fs::write(root.join("a").join("new.txt"), "x\n").unwrap();
            },
            |_| {},
        );
        h.settle();
        let a = h.root.join("a");
        git(&a, &["add", "new.txt"]);
        git(&a, &["commit", "-q", "-m", "add"]);
        let clean = h.wait_for(
            WAIT,
            |u| matches!(u, Update::Status(s) if s.path == a && !s.dirty && s.error.is_none()),
        );
        assert!(clean.is_some(), "no clean status after the commit");
    }

    #[test]
    fn new_and_removed_repos() {
        let h = Harness::new("repos", |root| init_repo(&root.join("a")), |_| {});
        h.settle();
        let a = h.root.join("a");
        let b = h.root.join("b");
        git(&h.root, &["init", "-q", "b"]);
        let added = h.wait_for(WAIT, |u| matches!(u, Update::Repos { .. }));
        assert!(
            matches!(&added, Some(Update::Repos { repos: r, .. }) if *r == vec![a.clone(), b.clone()]),
            "{added:?}"
        );
        // Moving it out of the root is a removal there, and works on every OS
        // without fighting read-only object files.
        std::fs::rename(&b, h.dir.join("b-moved")).unwrap();
        let removed = h.wait_for(WAIT, |u| matches!(u, Update::Repos { .. }));
        assert!(
            matches!(&removed, Some(Update::Repos { repos: r, .. }) if *r == vec![a.clone()]),
            "{removed:?}"
        );
    }

    #[test]
    fn finds_repos_cloned_deeper_down() {
        let h = Harness::new(
            "deep",
            |root| {
                init_repo(&root.join("a"));
                std::fs::create_dir_all(root.join("group")).unwrap();
            },
            |_| {},
        );
        h.settle();
        let a = h.root.join("a");
        let b = h.root.join("group").join("b");
        git(
            &h.root.join("group"),
            &["clone", "-q", a.to_str().unwrap(), "b"],
        );
        let repos = h.wait_for(WAIT, |u| matches!(u, Update::Repos { .. }));
        assert!(
            matches!(&repos, Some(Update::Repos { repos: r, .. }) if *r == vec![a.clone(), b.clone()]),
            "{repos:?}"
        );
        let status = h
            .status("group/b", WAIT)
            .expect("the new repo was never checked");
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(status.name, format!("group{sep}b"));
        assert!(!status.dirty, "{status:?}");
        // Its edits route to it.
        std::fs::write(b.join("README"), "changed\n").unwrap();
        let status = h.status("group/b", WAIT).expect("no status after an edit");
        assert!(status.dirty, "{status:?}");

        // A folder and a repo inside it at once, before any rediscovery:
        // the folder's own event is enough.
        let c = h.root.join("fresh").join("x").join("c");
        std::fs::create_dir_all(c.parent().unwrap()).unwrap();
        git(c.parent().unwrap(), &["init", "-q", "c"]);
        let found = h.wait_for(
            WAIT,
            |u| matches!(u, Update::Repos { repos, .. } if repos.contains(&c)),
        );
        assert!(found.is_some(), "a repo three levels down was not found");
    }

    #[test]
    fn churn_outside_repos_runs_no_git() {
        let h = Harness::new(
            "churn",
            |root| {
                init_repo(&root.join("a"));
                std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
                std::fs::create_dir_all(root.join("docs/x/y")).unwrap();
            },
            |_| {},
        );
        h.settle();
        for i in 0..20 {
            // Inside a skipped folder, below the depth limit, and plain files.
            std::fs::create_dir_all(h.root.join(format!("node_modules/pkg/d{i}"))).unwrap();
            std::fs::create_dir_all(h.root.join(format!("docs/x/y/z{i}"))).unwrap();
            std::fs::write(h.root.join(format!("docs/x/n{i}.txt")), "x").unwrap();
        }
        let updates = h.collect(Duration::from_millis(2500));
        assert!(started(&updates).is_empty(), "{updates:?}");
        assert!(
            !updates.iter().any(|u| matches!(u, Update::Repos { .. })),
            "{updates:?}"
        );
    }

    #[test]
    fn steady_folder_churn_still_finds_new_repos() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let h = Harness::new(
            "steady",
            |root| {
                init_repo(&root.join("a"));
                std::fs::create_dir_all(root.join("tmp")).unwrap();
            },
            |_| {},
        );
        h.settle();
        // A folder appearing every 200ms: each one asks for a walk, and
        // each would push a purely trailing window back again.
        let stop = Arc::new(AtomicBool::new(false));
        let churn = {
            let (stop, tmp) = (stop.clone(), h.root.join("tmp"));
            thread::spawn(move || {
                let mut i = 0;
                while !stop.load(Ordering::Relaxed) && i < 150 {
                    let _ = std::fs::create_dir(tmp.join(format!("d{i}")));
                    i += 1;
                    thread::sleep(Duration::from_millis(200));
                }
            })
        };
        thread::sleep(Duration::from_millis(500));
        let b = h.root.join("b");
        git(&h.root, &["init", "-q", "b"]);
        let found = h.wait_for(
            REPOSET_MAX_WAIT + WAIT,
            |u| matches!(u, Update::Repos { repos, .. } if repos.contains(&b)),
        );
        stop.store(true, Ordering::Relaxed);
        churn.join().unwrap();
        assert!(found.is_some(), "the new repo waited for the churn to stop");
    }

    #[test]
    fn depth_change_rediscovers() {
        let h = Harness::new(
            "depth",
            |root| {
                init_repo(&root.join("a"));
                init_repo(&root.join("group").join("b"));
            },
            |c| c.depth = 1,
        );
        let a = h.root.join("a");
        let b = h.root.join("group").join("b");
        let first = h.wait_for(WAIT, |u| matches!(u, Update::Repos { .. }));
        assert!(
            matches!(&first, Some(Update::Repos { repos: r, .. }) if *r == vec![a.clone()]),
            "{first:?}"
        );
        h.settle();
        let mut config = Config::load_or_create(&h.config_file).unwrap();
        config.depth = 2;
        write_config(&h.config_file, &config);
        let repos = h.wait_for(WAIT, |u| matches!(u, Update::Repos { .. }));
        assert!(
            matches!(&repos, Some(Update::Repos { repos: r, .. }) if *r == vec![a.clone(), b.clone()]),
            "{repos:?}"
        );
        let sweep = h.until_sweep_done();
        assert!(
            sweep
                .iter()
                .any(|u| matches!(u, Update::Started { repo, fetch: true } if *repo == b)),
            "{sweep:?}"
        );
    }

    #[test]
    fn ignored_folder_churn_is_free() {
        let h = Harness::new(
            "ignored",
            |root| {
                let a = root.join("a");
                init_repo(&a);
                std::fs::write(a.join(".gitignore"), "target/\n").unwrap();
                git(&a, &["add", ".gitignore"]);
                git(&a, &["commit", "-q", "-m", "ignore"]);
                std::fs::create_dir_all(a.join("target/debug")).unwrap();
            },
            |_| {},
        );
        h.settle();
        let target = h.root.join("a/target");
        for i in 0..20 {
            std::fs::write(target.join(format!("debug/{i}.o")), "obj").unwrap();
        }
        std::fs::create_dir_all(target.join("release")).unwrap();
        std::fs::write(target.join("release/x"), "x").unwrap();
        // Long enough for a wrongly routed event to clear MIN_GAP and quiet.
        let updates = h.collect(Duration::from_millis(2500));
        assert!(started(&updates).is_empty(), "{updates:?}");
        // The watch itself still works.
        std::fs::write(h.root.join("a/src.txt"), "x").unwrap();
        assert!(
            h.wait_for(WAIT, |u| matches!(u, Update::Started { .. }))
                .is_some()
        );
    }

    #[test]
    fn no_feedback_loop_after_checks_and_fetches() {
        let h = Harness::new(
            "loop",
            |root| {
                let origin = root.parent().unwrap().join("origin.git");
                let seed = root.parent().unwrap().join("seed");
                init_repo(&seed);
                git(
                    root,
                    &[
                        "clone",
                        "-q",
                        "--bare",
                        seed.to_str().unwrap(),
                        origin.to_str().unwrap(),
                    ],
                );
                git(root, &["clone", "-q", origin.to_str().unwrap(), "a"]);
            },
            |_| {},
        );
        h.settle();
        h.send(Command::RefreshAll);
        let a = h.root.join("a");
        let refresh = h.until_sweep_done();
        assert_eq!(
            started(&refresh).len(),
            1,
            "one fetch for the one repo: {refresh:?}"
        );
        assert!(
            refresh
                .iter()
                .any(|u| matches!(u, Update::Status(s) if s.path == a && s.is_synced()))
        );
        // ReadDirectoryChangesW can deliver the fetch's own ref writes after
        // it ends, which buys one local recheck (a push from a terminal looks
        // the same). A loop would keep going, so the recheck must be the last.
        let updates = h.collect(Duration::from_secs(3));
        let rechecks = started(&updates);
        assert!(
            rechecks.len() <= 1
                && rechecks
                    .iter()
                    .all(|u| matches!(u, Update::Started { fetch: false, .. })),
            "{updates:?}"
        );
        if !rechecks.is_empty() {
            let after = h.collect(Duration::from_secs(3));
            assert!(started(&after).is_empty(), "{after:?}");
        }
    }

    #[test]
    fn our_own_folder_writes_are_ignored() {
        let h = Harness::new("ini", |root| init_repo(&root.join("a")), |_| {});
        h.settle();
        let a = h.root.join("a");
        std::fs::write(a.join("desktop.ini"), "[.ShellClassInfo]\n").unwrap();
        // What PathMakeSystemFolder does: an attribute change on the folder.
        let original = std::fs::metadata(&a).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&a, readonly).unwrap();
        std::fs::set_permissions(&a, original).unwrap();
        let updates = h.collect(Duration::from_millis(2500));
        assert!(started(&updates).is_empty(), "{updates:?}");
    }

    #[test]
    fn config_rewrite_reloads_once() {
        let h = Harness::new("config", |root| init_repo(&root.join("a")), |_| {});
        h.settle();
        let mut config = Config::load_or_create(&h.config_file).unwrap();
        config.interval_minutes = 7.0;
        // An editor-style save: several writes in a burst.
        write_config(&h.config_file, &config);
        write_config(&h.config_file, &config);
        let updates = h.collect(Duration::from_millis(1500));
        let reloads: Vec<_> = updates
            .iter()
            .filter(|u| matches!(u, Update::Config(_)))
            .collect();
        assert_eq!(reloads.len(), 1, "{updates:?}");
        assert!(matches!(reloads[0], Update::Config(Ok(c)) if c.interval_minutes == 7.0));
        assert!(started(&updates).is_empty(), "same root: nothing to check");
    }

    #[test]
    fn root_change_moves_to_the_new_folder() {
        let h = Harness::new("reroot", |root| init_repo(&root.join("a")), |_| {});
        h.settle();
        let other = h.dir.join("other");
        init_repo(&other.join("b"));
        let b = other.join("b");
        let mut config = Config::load_or_create(&h.config_file).unwrap();
        config.path = other.to_string_lossy().into_owned();
        write_config(&h.config_file, &config);
        let repos = h.wait_for(WAIT, |u| matches!(u, Update::Repos { .. }));
        assert!(
            matches!(&repos, Some(Update::Repos { repos: r, .. }) if *r == vec![b.clone()]),
            "{repos:?}"
        );
        let sweep = h.until_sweep_done();
        assert!(
            sweep
                .iter()
                .any(|u| matches!(u, Update::Started { repo, fetch: true } if *repo == b)),
            "{sweep:?}"
        );
        let a = h.root.join("a");
        assert!(
            !sweep
                .iter()
                .any(|u| matches!(u, Update::Status(s) if s.path == a)),
            "{sweep:?}"
        );
        // The old root is no longer watched.
        std::fs::write(a.join("README"), "changed\n").unwrap();
        let updates = h.collect(Duration::from_millis(1500));
        assert!(started(&updates).is_empty(), "{updates:?}");
    }

    #[test]
    fn watch_off_and_back_on() {
        let h = Harness::new(
            "off",
            |root| init_repo(&root.join("a")),
            |c| c.watch = false,
        );
        assert!(
            h.wait_for(WAIT, |u| matches!(u, Update::Mode(WatchMode::Off)))
                .is_some(),
            "no Mode(Off)"
        );
        h.settle();
        let mut config = Config::load_or_create(&h.config_file).unwrap();
        config.watch = true;
        write_config(&h.config_file, &config);
        assert!(
            h.wait_for(WAIT, |u| matches!(u, Update::Mode(WatchMode::Events)))
                .is_some(),
            "config edit didn't turn watching on"
        );
    }

    #[test]
    fn applied_config_works_while_the_file_is_broken() {
        let h = Harness::new("apply", |root| init_repo(&root.join("a")), |_| {});
        h.settle();
        let mut config = Config::load_or_create(&h.config_file).unwrap();
        std::fs::write(&h.config_file, "{ mid-edit").unwrap();
        assert!(
            h.wait_for(WAIT, |u| matches!(u, Update::Config(Err(_))))
                .is_some(),
            "no reload error"
        );
        // A menu toggle that couldn't be saved.
        config.watch = false;
        h.send(Command::Apply(config));
        assert!(
            h.wait_for(WAIT, |u| matches!(u, Update::Mode(WatchMode::Off)))
                .is_some(),
            "the in-memory watch toggle didn't reach the monitor"
        );
    }

    #[test]
    fn sweeps_find_new_repos_without_watching() {
        let h = Harness::new(
            "sweep-discovery",
            |root| init_repo(&root.join("a")),
            |c| c.watch = false,
        );
        h.settle();
        let (a, b) = (h.root.join("a"), h.root.join("b"));
        init_repo(&b);
        h.send(Command::RefreshAll);
        let repos = h.wait_for(WAIT, |u| matches!(u, Update::Repos { .. }));
        assert!(
            matches!(&repos, Some(Update::Repos { repos: r, .. }) if *r == vec![a.clone(), b.clone()]),
            "{repos:?}"
        );
        let sweep = h.until_sweep_done();
        assert!(
            sweep
                .iter()
                .any(|u| matches!(u, Update::Started { repo, fetch: true } if *repo == b)),
            "{sweep:?}"
        );
    }

    #[test]
    fn missing_root_polls() {
        let h = Harness::new(
            "missing",
            |_| {},
            |c| c.path = "/definitely/not/here/gitstat".into(),
        );
        let mode = h.wait_for(WAIT, |u| matches!(u, Update::Mode(_)));
        assert!(
            matches!(mode, Some(Update::Mode(WatchMode::Polling { .. }))),
            "{mode:?}"
        );
        // An empty sweep still completes, so the UI can show "folder not found".
        h.settle();
    }
}
