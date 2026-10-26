//! Robustesse du shell (LAST-132) : erreurs journalisées au lieu de paniques, fils de service suivis et arrêtés
//! proprement, budget de relance (mpv).
//!
//! Journal : stderr (visible en débogage) et `%APPDATA%\Laststream\shell-errors.log` (borné), car en version finale
//! le shell est une application fenêtrée sans console. Sentry n'est pas intégré au shell (aucune dépendance) :
//! `report_error` est le point d'accroche unique pour le brancher.

use std::{
    any::Any,
    collections::VecDeque,
    fmt::Display,
    fs::{self, OpenOptions},
    io::Write,
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const LOG_FILE: &str = "shell-errors.log";
/// Au-delà, le journal repart de zéro.
const LOG_MAX_BYTES: u64 = 256 * 1024;
/// Pas d'attente des boucles qui surveillent l'arrêt.
const POLL: Duration = Duration::from_millis(200);

fn log_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Laststream")
        .join(LOG_FILE)
}

fn append_to_log(line: &str) {
    if cfg!(test) {
        return;
    }
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_MAX_BYTES) {
        let _ = fs::remove_file(&path);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

/// Journalise une erreur de production (contexte court, sans donnée de l'utilisateur).
pub fn report_error(context: &str, error: &dyn Display) {
    let line = format!("{context}: {error}");
    eprintln!("[laststream] {line}");
    append_to_log(&line);
}

/// Journalise les paniques (lieu seulement) : en version finale elles seraient silencieuses.
pub fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_else(|| "?".to_owned());
        let thread = thread::current();
        report_error(
            "panic",
            &format!("fil {} à {location}", thread.name().unwrap_or("sans nom")),
        );
        previous(info);
    }));
}

/// Verrouille un mutex même empoisonné par une panique d'un autre fil : la valeur est reprise et l'incident journalisé.
pub fn lock<'a, T>(mutex: &'a Mutex<T>, what: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        report_error(what, &"verrou empoisonné, valeur reprise");
        poisoned.into_inner()
    })
}

/// `result.or_report("contexte")?` : journalise l'erreur puis la laisse remonter.
pub trait OrReport<T, E> {
    fn or_report(self, context: &str) -> Result<T, E>;
}

impl<T, E: Display> OrReport<T, E> for Result<T, E> {
    fn or_report(self, context: &str) -> Result<T, E> {
        if let Err(error) = &self {
            report_error(context, error);
        }
        self
    }
}

/// Texte d'une charge de panique.
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panique sans message".to_owned())
}

/// Attend la fin d'un fil au plus `timeout`. `true` si le fil s'est terminé (et a été joint).
pub fn join_with_timeout(handle: JoinHandle<()>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = handle.join();
    true
}

/// Fils de service d'une instance : drapeau d'arrêt partagé et liste des fils à joindre à la fermeture.
pub struct Services {
    shutdown: AtomicBool,
    threads: Mutex<Vec<(String, JoinHandle<()>)>>,
}

impl Services {
    pub const fn new() -> Self {
        Self {
            shutdown: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
        }
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Lance un fil suivi : une panique est attrapée et journalisée, le fil est joint par `shutdown`.
    pub fn spawn<F: FnOnce() + Send + 'static>(&self, name: &str, body: F) {
        let thread_name = name.to_owned();
        let spawned = thread::Builder::new().name(name.to_owned()).spawn(move || {
            if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(body)) {
                report_error(
                    &format!("fil {thread_name} arrêté par une panique"),
                    &panic_message(payload.as_ref()),
                );
            }
        });
        match spawned {
            Ok(handle) => {
                let mut threads = lock(&self.threads, "registre des fils");
                threads.retain(|(_, handle)| !handle.is_finished());
                threads.push((name.to_owned(), handle));
            }
            Err(error) => report_error(&format!("fil {name} non démarré"), &error),
        }
    }

    /// Attend un message en surveillant l'arrêt. `None` : canal fermé ou arrêt demandé.
    pub fn recv<T>(&self, receiver: &flume::Receiver<T>) -> Option<T> {
        loop {
            if self.is_shutdown() {
                return None;
            }
            match receiver.recv_timeout(POLL) {
                Ok(message) => return Some(message),
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Dort `duration` par tranches ; `false` si l'arrêt a été demandé entre-temps.
    pub fn sleep(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        while !self.is_shutdown() {
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            thread::sleep((deadline - now).min(POLL));
        }
        false
    }

    /// Demande l'arrêt : les boucles de `recv` et `sleep` rendent la main.
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// Joint les fils pendant `timeout` au total. Renvoie le nombre de fils encore actifs.
    pub fn join_all(&self, timeout: Duration) -> usize {
        let threads = std::mem::take(&mut *lock(&self.threads, "registre des fils"));
        let deadline = Instant::now() + timeout;
        let mut stragglers = 0;
        for (name, handle) in threads {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !join_with_timeout(handle, remaining) {
                report_error(&format!("fil {name}"), &"toujours actif à la fermeture");
                stragglers += 1;
            }
        }
        stragglers
    }
}

/// Fils de service du shell.
pub static SERVICES: Services = Services::new();

/// Budget de relance : au plus `max_attempts` relances sur une fenêtre glissante, avec attente exponentielle.
pub struct RestartPolicy {
    max_attempts: usize,
    window: Duration,
    base_delay: Duration,
    failures: VecDeque<Instant>,
}

impl RestartPolicy {
    pub fn new(max_attempts: usize, window: Duration, base_delay: Duration) -> Self {
        Self {
            max_attempts,
            window,
            base_delay,
            failures: VecDeque::new(),
        }
    }

    /// Enregistre un arrêt inattendu à `now`. `Some(attente)` avant de relancer, `None` si le budget est épuisé.
    pub fn on_failure(&mut self, now: Instant) -> Option<Duration> {
        while self
            .failures
            .front()
            .is_some_and(|first| now.saturating_duration_since(*first) > self.window)
        {
            self.failures.pop_front();
        }
        if self.failures.len() >= self.max_attempts {
            return None;
        }
        let exponent = self.failures.len().min(5) as u32;
        self.failures.push_back(now);
        Some(self.base_delay * 2u32.pow(exponent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn restart_budget_allows_three_attempts_then_stops() {
        let mut policy = RestartPolicy::new(3, Duration::from_secs(60), Duration::from_millis(250));
        let start = Instant::now();
        assert_eq!(policy.on_failure(start), Some(Duration::from_millis(250)));
        assert_eq!(
            policy.on_failure(start + Duration::from_secs(1)),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            policy.on_failure(start + Duration::from_secs(2)),
            Some(Duration::from_millis(1000))
        );
        assert_eq!(policy.on_failure(start + Duration::from_secs(3)), None);
    }

    #[test]
    fn restart_budget_recovers_once_the_window_has_passed() {
        let mut policy = RestartPolicy::new(2, Duration::from_secs(60), Duration::from_millis(100));
        let start = Instant::now();
        assert!(policy.on_failure(start).is_some());
        assert!(policy.on_failure(start + Duration::from_secs(1)).is_some());
        assert!(policy.on_failure(start + Duration::from_secs(2)).is_none());
        // La première panne sort de la fenêtre : une relance redevient possible, avec l'attente initiale.
        assert_eq!(
            policy.on_failure(start + Duration::from_secs(61)),
            Some(Duration::from_millis(200))
        );
    }

    #[test]
    fn poisoned_mutex_is_recovered_instead_of_panicking() {
        let mutex = Arc::new(Mutex::new(41));
        let poisoner = Arc::clone(&mutex);
        let _ = thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("empoisonne le verrou");
        })
        .join();
        assert!(mutex.is_poisoned());
        *lock(&mutex, "test") += 1;
        assert_eq!(*lock(&mutex, "test"), 42);
    }

    #[test]
    fn spawned_service_panic_is_caught_and_shutdown_joins_everything() {
        let services = Services::new();
        services.spawn("panique", || panic!("boum"));
        let (tx, rx) = flume::unbounded::<u8>();
        let services = Arc::new(services);
        let waiter = Arc::clone(&services);
        services.spawn("attente", move || {
            // Sort quand l'arrêt est demandé, sans qu'aucun message n'arrive.
            assert_eq!(waiter.recv(&rx), None);
        });
        services.request_shutdown();
        assert_eq!(services.join_all(Duration::from_secs(5)), 0);
        drop(tx);
    }

    #[test]
    fn shutdown_reports_a_thread_that_does_not_stop() {
        let services = Services::new();
        let (release, blocked) = flume::bounded::<()>(1);
        services.spawn("bloque", move || {
            let _ = blocked.recv();
        });
        services.request_shutdown();
        assert_eq!(services.join_all(Duration::from_millis(100)), 1);
        release.send(()).unwrap();
    }

    #[test]
    fn sleep_returns_early_on_shutdown() {
        let services = Arc::new(Services::new());
        let sleeper = Arc::clone(&services);
        let handle = thread::spawn(move || sleeper.sleep(Duration::from_secs(30)));
        thread::sleep(Duration::from_millis(50));
        services.request_shutdown();
        assert!(!handle.join().unwrap());
    }

    #[test]
    fn panic_message_reads_str_and_string_payloads() {
        let from_str = panic::catch_unwind(|| panic!("texte")).unwrap_err();
        assert_eq!(panic_message(from_str.as_ref()), "texte");
        let from_string =
            panic::catch_unwind(|| panic!("{}", String::from("formaté"))).unwrap_err();
        assert_eq!(panic_message(from_string.as_ref()), "formaté");
    }
}
