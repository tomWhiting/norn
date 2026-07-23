//! Explicit browser-login presentation and launch boundary.

use super::super::browser::{BrowserLaunch, BrowserLaunchError};
use super::super::login_prompt::{LoginPrompt, LoginPromptPresenter};
use super::{LoginError, map_browser_launch_error};

pub(super) struct PreparedBrowserLaunch {
    pub(super) browser_launch: Option<BrowserLaunch>,
    pub(super) manual_fallback: bool,
}

pub(super) fn present_and_open_browser<F>(
    presenter: Option<&dyn LoginPromptPresenter>,
    authorize_url: &url::Url,
    open_browser: F,
) -> Result<PreparedBrowserLaunch, LoginError>
where
    F: FnOnce(&url::Url) -> Result<BrowserLaunch, BrowserLaunchError>,
{
    let manual_fallback = presenter.is_some();
    if let Some(presenter) = presenter {
        presenter
            .present(LoginPrompt::Browser {
                authorization_url: authorize_url.as_str(),
            })
            .map_err(|_error| LoginError::Presentation)?;
    }
    match open_browser(authorize_url) {
        Ok(browser_launch) => Ok(PreparedBrowserLaunch {
            browser_launch: Some(browser_launch),
            manual_fallback,
        }),
        Err(_error) if manual_fallback => Ok(PreparedBrowserLaunch {
            browser_launch: None,
            manual_fallback: true,
        }),
        Err(error) => Err(map_browser_launch_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;

    use super::*;
    use crate::provider::openai_oauth::login_prompt::LoginPromptError;

    #[derive(Default)]
    struct CapturingPresenter {
        urls: Mutex<Vec<String>>,
    }

    impl LoginPromptPresenter for CapturingPresenter {
        fn present(&self, prompt: LoginPrompt<'_>) -> Result<(), LoginPromptError> {
            let LoginPrompt::Browser { authorization_url } = prompt else {
                return Err(LoginPromptError::terminal_output_unavailable());
            };
            self.urls.lock().push(authorization_url.to_owned());
            Ok(())
        }
    }

    #[test]
    fn exact_url_is_presented_before_a_failed_desktop_launch()
    -> Result<(), Box<dyn std::error::Error>> {
        let presenter = CapturingPresenter::default();
        let authorize_url = super::super::build_authorize_url(
            "client-id",
            "http://localhost:1455/auth/callback",
            "pkce-challenge",
            "csrf-state",
        )?;

        let prepared = present_and_open_browser(Some(&presenter), &authorize_url, |target| {
            assert_eq!(presenter.urls.lock().as_slice(), [target.as_str()]);
            Err(BrowserLaunchError::Structural("desktop opener unavailable"))
        })?;

        assert!(prepared.browser_launch.is_none());
        assert!(prepared.manual_fallback);
        assert_eq!(presenter.urls.lock().as_slice(), [authorize_url.as_str()]);
        Ok(())
    }

    #[test]
    fn failed_desktop_launch_without_presented_fallback_is_typed()
    -> Result<(), Box<dyn std::error::Error>> {
        let authorize_url = url::Url::parse("https://auth.openai.com/oauth/authorize")?;

        let result = present_and_open_browser(None, &authorize_url, |_target| {
            Err(BrowserLaunchError::Structural("desktop opener unavailable"))
        });

        assert!(matches!(
            result,
            Err(LoginError::Browser("desktop opener unavailable"))
        ));
        Ok(())
    }
}
