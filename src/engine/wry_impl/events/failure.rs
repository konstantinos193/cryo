//! WebView2 transport status classification.
use crate::engine::LoadFailure;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
/// WebView2 status → our vocabulary. `None` for the statuses the shell must
/// not paint over: a cancelled load is the user changing their mind, and an
/// authentication challenge is a dialog the engine raises itself.
pub(super) fn load_failure(status: COREWEBVIEW2_WEB_ERROR_STATUS) -> Option<LoadFailure> {
    Some(match status {
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED => LoadFailure::Offline,
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED => LoadFailure::HostNotFound,
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT => LoadFailure::TimedOut,
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET =>
        {
            LoadFailure::ConnectionFailed
        }
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS =>
        {
            LoadFailure::CertificateRejected
        }
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_REDIRECT_FAILED => LoadFailure::RedirectLoop,
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_ERROR_HTTP_INVALID_SERVER_RESPONSE => {
            LoadFailure::MalformedResponse
        }
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_VALID_AUTHENTICATION_CREDENTIALS_REQUIRED
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_VALID_PROXY_AUTHENTICATION_REQUIRED =>
        {
            return None
        }
        _ => LoadFailure::Unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_statuses_map_onto_our_vocabulary() {
        let cases = [
            (
                COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED,
                LoadFailure::Offline,
            ),
            (
                COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED,
                LoadFailure::HostNotFound,
            ),
            (COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT, LoadFailure::TimedOut),
            (
                COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT,
                LoadFailure::ConnectionFailed,
            ),
            (
                COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET,
                LoadFailure::ConnectionFailed,
            ),
            (
                COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED,
                LoadFailure::CertificateRejected,
            ),
            (
                COREWEBVIEW2_WEB_ERROR_STATUS_REDIRECT_FAILED,
                LoadFailure::RedirectLoop,
            ),
            (
                COREWEBVIEW2_WEB_ERROR_STATUS_ERROR_HTTP_INVALID_SERVER_RESPONSE,
                LoadFailure::MalformedResponse,
            ),
        ];
        for (status, expected) in cases {
            assert_eq!(load_failure(status), Some(expected), "{status:?}");
        }
    }

    #[test]
    fn cancels_and_auth_challenges_paint_no_error_page() {
        for status in [
            COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED,
            COREWEBVIEW2_WEB_ERROR_STATUS_VALID_AUTHENTICATION_CREDENTIALS_REQUIRED,
            COREWEBVIEW2_WEB_ERROR_STATUS_VALID_PROXY_AUTHENTICATION_REQUIRED,
        ] {
            assert_eq!(load_failure(status), None, "{status:?}");
        }
    }

    #[test]
    fn an_unrecognised_status_reports_unknown_rather_than_disappearing() {
        assert_eq!(
            load_failure(COREWEBVIEW2_WEB_ERROR_STATUS(0x7fff)),
            Some(LoadFailure::Unknown)
        );
    }
}
