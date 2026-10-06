use super::*;
use std::cell::{Cell, RefCell};

struct Service {
    state: Cell<isize>,
    approval: bool,
    fail: bool,
    calls: RefCell<Vec<&'static str>>,
}
impl Service {
    fn new(state: isize) -> Self {
        Self {
            state: Cell::new(state),
            approval: false,
            fail: false,
            calls: RefCell::new(vec![]),
        }
    }
}
impl AppService for Service {
    fn state(&self) -> isize {
        self.state.get()
    }
    fn register(&self) -> Result<()> {
        self.calls.borrow_mut().push("register");
        if self.fail {
            bail!("SERVICE_FAILED: simulated registration error");
        }
        self.state.set(if self.approval { 2 } else { 1 });
        Ok(())
    }
    fn unregister(&self) -> Result<()> {
        self.calls.borrow_mut().push("unregister");
        if self.fail {
            bail!("SERVICE_FAILED: simulated removal error");
        }
        self.state.set(0);
        Ok(())
    }
}

#[test]
fn approval_and_failed_registration_never_report_a_running_service() -> Result<()> {
    for agent in 0..=3 {
        for main in 0..=3 {
            let status = service_state(&Service::new(agent), &Service::new(main), true);
            assert_eq!(status.installed, matches!(agent, 1 | 2));
            assert_eq!(status.app_at_login, matches!(main, 1 | 2));
            assert_eq!(status.approval_required, agent == 2 || main == 2);
            assert!(status.legacy_installed && !status.development);
        }
    }
    let mut service = Service::new(0);
    service.fail = true;
    assert!(
        enable_agent(&service)
            .unwrap_err()
            .to_string()
            .starts_with("SERVICE_FAILED:")
    );
    assert_eq!(service.state(), 0);
    service.fail = false;
    service.approval = true;
    assert!(
        enable_agent(&service)
            .unwrap_err()
            .to_string()
            .starts_with("APPROVAL_REQUIRED:")
    );
    assert_eq!(service.state(), 2);
    service.state.set(1);
    service.calls.borrow_mut().clear();
    enable_agent(&service)?;
    assert!(
        service.calls.borrow().is_empty(),
        "already-enabled agents must not be registered again"
    );
    Ok(())
}

#[test]
fn login_registration_is_idempotent_and_failed_removal_remains_registered() -> Result<()> {
    for initial in 0..=3 {
        let mut service = Service::new(initial);
        set_enabled(&service, true)?;
        let calls = service.calls.borrow().len();
        set_enabled(&service, true)?;
        assert_eq!(service.calls.borrow().len(), calls);
        assert_eq!(calls, usize::from(!matches!(initial, 1 | 2)));
        service.fail = true;
        assert!(set_enabled(&service, false).is_err());
        assert!(matches!(service.state(), 1 | 2));
        service.fail = false;
        set_enabled(&service, false)?;
        assert_eq!(service.state(), 0);
        let calls = service.calls.borrow().len();
        set_enabled(&service, false)?;
        assert_eq!(service.calls.borrow().len(), calls);
    }
    for path in [
        "/",
        "xrun",
        "/tmp/MacOS/xrun",
        "/tmp/example/Contents/MacOS/xrun",
        "/tmp/example.app/Other/MacOS/xrun",
    ] {
        assert!(!is_bundle_executable(std::path::Path::new(path)), "{path}");
    }
    assert!(is_bundle_executable(std::path::Path::new(
        "/tmp/example.app/Contents/MacOS/xrun"
    )));
    assert!(
        register_agent()
            .unwrap_err()
            .to_string()
            .starts_with("APP_BUNDLE_REQUIRED:")
    );
    Ok(())
}
