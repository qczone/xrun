import { useCallback, useRef, useState } from "react";
import type { DesktopPage } from "./api";
import { Icon, IconDefinitions, type IconName } from "./components/Icon";
import { ErrorNotice } from "./components/ErrorNotice";
import { LocalDeviceCard, StatusBanner } from "./components/LocalDevice";
import { t, useLanguage } from "./i18n";
import { Logo } from "./components/Logo";
import { Devices } from "./pages/Devices";
import { SettingsPage } from "./pages/Settings";
import { ActivityPage } from "./pages/Activity";
import { useStatus } from "./app/useStatus";
import { useWindowVisible } from "./app/useWindowVisible";
import { useTrayNavigation } from "./app/useTrayNavigation";
import { useDevices } from "./app/useDevices";
import { ConfirmationDialog, useConfirmation } from "./app/useConfirmation";
import {
  OperationsContext,
  useOperationController,
  type InteractionState,
} from "./app/useOperations";

export function App() {
  useLanguage();
  const pages: { id: DesktopPage; label: string; icon: IconName }[] = [
    { id: "devices", label: t("nav.devices"), icon: "devices" },
    { id: "activity", label: t("nav.activity"), icon: "activity" },
    { id: "settings", label: t("nav.settings"), icon: "settings" },
  ];
  const [page, setPage] = useState<DesktopPage>("devices");
  const [networkSettingsRequest, setNetworkSettingsRequest] = useState(0);
  const main = useRef<HTMLElement>(null);
  const interactionState = useRef<InteractionState>({
    busy: false,
    confirming: false,
  });
  const visible = useWindowVisible();
  const statusController = useStatus(interactionState, visible);
  const confirmation = useConfirmation(interactionState);
  const operations = useOperationController(
    page,
    statusController.refresh,
    confirmation.confirm,
    interactionState,
    statusController.clearError,
  );
  const { status, error: statusError } = statusController;
  const { busy, pending, error, toast } = operations;
  const members = useDevices(
    status,
    visible && page === "devices",
    busy || confirmation.active,
    visible,
  );
  const deviceCount = members.devices.filter(
    (device) => device.device_id !== status?.local.device_id && !device.revoked,
  ).length;
  const navigate = useCallback((next: DesktopPage) => {
    setPage(next);
    if (main.current) main.current.scrollTop = 0;
  }, []);
  useTrayNavigation(navigate);
  const openNetworkSettings = () => {
    navigate("settings");
    setNetworkSettingsRequest((value) => value + 1);
  };
  const feedback = (origin: DesktopPage) =>
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
              <span className="nav-label">{item.label}</span>
              {item.id === "devices" && deviceCount > 0 && (
                <span className="nav-count" aria-hidden="true">
                  {deviceCount}
                </span>
              )}
            </button>
          ))}
        </nav>
        {status?.local.joined && (
          <LocalDeviceCard
            status={status}
            openNetworkSettings={openNetworkSettings}
          />
        )}
      </aside>
      <div className="workspace">
        <main ref={main} aria-busy={busy}>
          {status?.local.joined && <StatusBanner status={status} />}
          {statusError && statusError !== error?.detail && (
            <ErrorNotice
              title={t("error.status")}
              detail={statusError}
              retry={() => void statusController.refresh()}
              dismiss={() => statusController.dismiss(statusError)}
            />
          )}
          {pending && (
            <span className="operation-progress sr-only" role="status">
              {t(`operation.${pending}`)}
            </span>
          )}
          <section
            id="page-devices"
            className="page"
            hidden={page !== "devices"}
          >
            <Devices
              active={visible && page === "devices"}
              paused={confirmation.active}
              status={status}
              members={members}
              feedback={feedback("devices")}
            />
          </section>
          <section
            id="page-activity"
            className="page"
            hidden={page !== "activity"}
          >
            <ActivityPage
              active={visible && page === "activity"}
              paused={confirmation.active}
              status={status}
              devices={members.devices}
              feedback={feedback("activity")}
            />
          </section>
          <section
            id="page-settings"
            className="page"
            hidden={page !== "settings"}
          >
            <SettingsPage
              status={status}
              feedback={feedback("settings")}
              networkSettingsRequest={networkSettingsRequest}
            />
          </section>
        </main>
        {toast && (
          <div id="toast" className="toast" role="status">
            <Icon name="check" />
            {toast}
          </div>
        )}
      </div>
      <ConfirmationDialog controller={confirmation} />
    </OperationsContext.Provider>
  );
}
