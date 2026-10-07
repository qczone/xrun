import { useRef, useState } from "react";
import { Icon, IconDefinitions, type IconName } from "./components/Icon";
import { ErrorNotice } from "./components/ErrorNotice";
import { serviceLabel } from "./format";
import { t, useLanguage } from "./i18n";
import { Logo } from "./components/Logo";
import { Overview } from "./pages/Overview";
import { Devices } from "./pages/Devices";
import { SettingsPage } from "./pages/Settings";
import { History } from "./pages/History";
import { useStatus } from "./app/useStatus";
import { useDevices } from "./app/useDevices";
import { ConfirmationDialog, useConfirmation } from "./app/useConfirmation";
import {
  OperationsContext,
  useOperationController,
  type Activity,
  type Page,
} from "./app/useOperations";
export function App() {
  useLanguage();
  const pages: { id: Page; label: string; icon: IconName }[] = [
    { id: "overview", label: t("nav.overview"), icon: "monitor" },
    { id: "devices", label: t("nav.devices"), icon: "devices" },
    { id: "history", label: t("nav.history"), icon: "terminal" },
    { id: "settings", label: t("nav.settings"), icon: "settings" },
  ];
  const [page, setPage] = useState<Page>("overview");
  const main = useRef<HTMLElement>(null);
  const activity = useRef<Activity>({ busy: false, confirming: false });
  const statusController = useStatus(activity);
  const confirmation = useConfirmation(activity);
  const operations = useOperationController(
    page,
    statusController.refresh,
    confirmation.confirm,
    activity,
    statusController.clearError,
  );
  const { status, error: statusError } = statusController;
  const { busy, pending, error, toast } = operations;
  const members = useDevices(
    status,
    page === "devices",
    busy || confirmation.active,
  );
  const { devices } = members;
  const navigate = (next: Page) => {
    setPage(next);
    if (main.current) main.current.scrollTop = 0;
  };
  const others = devices.filter(
    (device) => device.device_id !== status?.local.device_id,
  );
  const [badge] = serviceLabel(status);
  const stateTone =
    status?.local.remote_access_paused || status?.service.approval_required
      ? "warning"
      : status?.local.daemon_connected === true
        ? "online"
        : "";
  const feedback = (origin: Page) =>
    error?.page === origin ? (
      <ErrorNotice
        title={error.title}
        detail={error.detail}
        dismiss={() => {
          statusController.dismiss(error.detail);
          operations.clearError();
        }}
      />
    ) : null;

  return (
    <OperationsContext.Provider value={operations}>
      <IconDefinitions />
      <aside className="sidebar">
        <div className="brand">
          <Logo />
        </div>
        <nav aria-label={t("nav.main")}>
          {pages.map((item) => (
            <button
              key={item.id}
              className={`nav-item ${page === item.id ? "selected" : ""}`}
              data-page={item.id}
              aria-label={item.label}
              aria-current={page === item.id ? "page" : undefined}
              onClick={() => navigate(item.id)}
            >
              <Icon name={item.icon} />
              {item.label}
              {item.id === "devices" &&
                others.some((device) => !device.revoked) && (
                  <span className="nav-count">
                    {others.filter((device) => !device.revoked).length}
                  </span>
                )}
            </button>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <div className="sidebar-device">
            <span className={`dot ${stateTone}`} />
            <div>
              <strong>{status?.local.name || t("common.localDevice")}</strong>
              <span>{badge}</span>
            </div>
          </div>
          {pending && (
            <span className="operation-progress" role="status">
              {t(`operation.${pending}`)}
            </span>
          )}
          <span className="version">
            {status && `v${status.local.version}`}
          </span>
        </div>
      </aside>
      <div className="workspace">
        <main ref={main} aria-busy={busy}>
          {statusError && statusError !== error?.detail && (
            <ErrorNotice
              title={t("error.status")}
              detail={statusError}
              retry={() => void statusController.refresh()}
              dismiss={() => {
                statusController.dismiss(statusError);
              }}
            />
          )}
          <section
            id="page-overview"
            className="page"
            hidden={page !== "overview"}
          >
            <Overview
              status={status}
              feedback={feedback("overview")}
              navigate={navigate}
            />
          </section>
          <section
            id="page-devices"
            className="page"
            hidden={page !== "devices"}
          >
            <Devices
              active={page === "devices"}
              status={status}
              members={members}
              feedback={feedback("devices")}
            />
          </section>
          <section
            id="page-history"
            className="page"
            hidden={page !== "history"}
          >
            <History
              active={page === "history"}
              paused={confirmation.active}
              status={status}
              devices={devices}
            />
          </section>
          <section
            id="page-settings"
            className="page"
            hidden={page !== "settings"}
          >
            <SettingsPage status={status} feedback={feedback("settings")} />
          </section>
        </main>
        {toast && (
          <div id="toast" className="toast" role="status">
            {toast}
          </div>
        )}
      </div>
      <ConfirmationDialog controller={confirmation} />
    </OperationsContext.Provider>
  );
}
