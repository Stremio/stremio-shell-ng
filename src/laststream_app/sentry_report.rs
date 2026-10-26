//! Envoi des erreurs et des paniques du shell à Sentry (LAST-172).
//!
//! Actif seulement si un DSN est fourni : variable d'exécution `LASTSTREAM_SENTRY_DSN`, à défaut valeur de compilation
//! (`option_env!`). Sans DSN, `init` renvoie `None` : aucun envoi, aucune erreur.
//!
//! Le DSN est public par nature : il ne permet que d'envoyer des événements, pas d'en lire, et il peut donc figurer
//! dans l'exécutable. Côté projet Sentry, activer le filtrage des domaines et des IP autorisés, et une limite de débit.
//!
//! Chaque événement passe par `scrub_event` avant envoi : textes masqués par `robust::scrub`, nom d'hôte, contexte
//! libre (`extra`), requête HTTP et données des fils d'Ariane retirés.

use std::{borrow::Cow, sync::Arc};

use super::robust::{report_error, scrub};

/// Variable lue à l'exécution pour fournir le DSN.
pub const DSN_ENV: &str = "LASTSTREAM_SENTRY_DSN";
/// DSN fourni à la compilation, s'il existe.
const COMPILED_DSN: Option<&str> = option_env!("LASTSTREAM_SENTRY_DSN");

/// Initialise Sentry si un DSN est configuré. Le guard vit jusqu'à la fin de `main` (vidage des envois à la fermeture).
pub fn init() -> Option<sentry::ClientInitGuard> {
    init_with_dsn(chosen_dsn(std::env::var(DSN_ENV).ok(), COMPILED_DSN))
}

/// La variable d'exécution l'emporte sur la valeur de compilation ; une valeur vide compte comme absente.
fn chosen_dsn(runtime: Option<String>, compiled: Option<&str>) -> Option<String> {
    runtime
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            compiled
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
        })
}

fn init_with_dsn(dsn: Option<String>) -> Option<sentry::ClientInitGuard> {
    let dsn = dsn?;
    let Ok(parsed) = dsn.trim().parse::<sentry::types::Dsn>() else {
        // Le texte du DSN n'est pas journalisé : il contient la clé du projet.
        report_error(
            "sentry: DSN invalide, envoi désactivé",
            &"format non reconnu",
        );
        return None;
    };
    let mut options = sentry::ClientOptions::default();
    options.release = Some(Cow::Borrowed(env!("CARGO_PKG_VERSION")));
    options.send_default_pii = false;
    options.before_send = Some(Arc::new(|event| Some(scrub_event(event))));
    let guard = sentry::init((parsed, options));
    if !guard.is_enabled() {
        return None;
    }
    sentry::configure_scope(|scope| {
        scope.set_tag("release", env!("CARGO_PKG_VERSION"));
        scope.set_tag("platform", "windows");
    });
    Some(guard)
}

/// Masque les textes d'un événement avant envoi (message, journal, exceptions, fil d'Ariane) et retire le nom d'hôte.
fn scrub_event(mut event: sentry::protocol::Event<'static>) -> sentry::protocol::Event<'static> {
    event.message = event.message.take().map(|text| scrub(&text));
    if let Some(entry) = event.logentry.as_mut() {
        entry.message = scrub(&entry.message);
    }
    for exception in &mut event.exception.values {
        exception.value = exception.value.take().map(|text| scrub(&text));
    }
    for crumb in &mut event.breadcrumbs.values {
        crumb.message = crumb.message.take().map(|text| scrub(&text));
        // Défense en profondeur : les données libres des fils d'Ariane ne partent pas.
        crumb.data.clear();
    }
    // L'app ne remplit ni le contexte libre ni la requête HTTP : ce qui s'y trouverait n'est pas maîtrisé.
    event.extra.clear();
    event.request = None;
    event.server_name = None;
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_dsn_nothing_is_configured_and_init_returns_none() {
        assert_eq!(chosen_dsn(None, None), None);
        assert_eq!(chosen_dsn(Some("  ".to_owned()), Some("")), None);
        assert!(init_with_dsn(None).is_none());
    }

    #[test]
    fn runtime_dsn_wins_over_the_compiled_one() {
        assert_eq!(
            chosen_dsn(Some("runtime".to_owned()), Some("compiled")),
            Some("runtime".to_owned())
        );
        assert_eq!(
            chosen_dsn(None, Some("compiled")),
            Some("compiled".to_owned())
        );
    }

    #[test]
    fn malformed_dsn_disables_sentry_without_panicking() {
        assert!(init_with_dsn(Some("pas une url".to_owned())).is_none());
    }

    #[test]
    fn scrub_event_masks_message_and_drops_the_host_name() {
        let event = sentry::protocol::Event {
            message: Some(
                "échec https://api.alldebrid.com/v4/user?agent=x&apikey=SECRET000".to_owned(),
            ),
            server_name: Some("poste-de-greg".into()),
            ..Default::default()
        };
        let scrubbed = scrub_event(event);
        assert_eq!(
            scrubbed.message.as_deref(),
            Some("échec https://api.alldebrid.com/[masqué]")
        );
        assert!(scrubbed.server_name.is_none());
    }

    #[test]
    fn scrub_event_drops_extra_request_and_breadcrumb_data() {
        let mut event = sentry::protocol::Event {
            request: Some(sentry::protocol::Request {
                data: Some("corps de requête".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        event.extra.insert("contexte".to_owned(), "valeur".into());
        let mut crumb = sentry::protocol::Breadcrumb::default();
        crumb.data.insert("token".to_owned(), "SECRET11".into());
        event.breadcrumbs.values.push(crumb);
        let scrubbed = scrub_event(event);
        assert!(scrubbed.extra.is_empty());
        assert!(scrubbed.request.is_none());
        assert!(scrubbed
            .breadcrumbs
            .values
            .iter()
            .all(|crumb| crumb.data.is_empty()));
    }
}
