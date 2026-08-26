use crate::i18n::Strings;
use crate::model::OperationKind;

pub fn finished(strings: &Strings, kind: OperationKind, cancelled: bool, errors: usize) {
    if cancelled {
        return;
    }
    let body = if errors == 0 {
        strings.notify_done(kind)
    } else {
        strings.notify_done_errors(kind, errors)
    };
    let _ = winrt_notification::Toast::new(winrt_notification::Toast::POWERSHELL_APP_ID)
        .title(strings.app_title)
        .text1(&body)
        .show();
}
