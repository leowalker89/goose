import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { IconPuzzle, IconSearch } from "@tabler/icons-react";
import { useChatSessionStore } from "@/features/chat/stores/chatSessionStore";
import { Input } from "@/shared/ui/input";
import { Badge } from "@/shared/ui/badge";
import { Spinner } from "@/shared/ui/spinner";
import { Widget } from "./Widget";
import { listSessionExtensionStatuses } from "@/features/extensions/api/extensions";
import {
  getDisplayName,
  type ExtensionStatus,
  type ExtensionStatusEntry,
} from "@/features/extensions/types";

function ExtensionStatusBadge({
  status,
  label,
}: {
  status: ExtensionStatus;
  label: string;
}) {
  if (status === "loading") {
    return (
      <Badge variant="secondary" className="gap-1 text-muted-foreground">
        <Spinner className="size-3" />
        {label}
      </Badge>
    );
  }

  return (
    <Badge
      variant="secondary"
      className={status === "connected" ? "text-success" : "text-danger"}
    >
      {label}
    </Badge>
  );
}

export function ExtensionsWidget() {
  const { t } = useTranslation("chat");
  const activeSessionId = useChatSessionStore((s) => s.activeSessionId);
  const [extensions, setExtensions] = useState<ExtensionStatusEntry[]>([]);
  const [searchTerm, setSearchTerm] = useState("");

  const fetchExtensions = useCallback(async () => {
    if (!activeSessionId) {
      setExtensions([]);
      return;
    }

    try {
      const all = await listSessionExtensionStatuses(activeSessionId);
      setExtensions(all);
    } catch {
      setExtensions([]);
    }
  }, [activeSessionId]);

  useEffect(() => {
    void fetchExtensions();
    const handleVisibility = () => {
      if (document.visibilityState === "visible") {
        void fetchExtensions();
      }
    };
    document.addEventListener("visibilitychange", handleVisibility);
    window.addEventListener("focus", fetchExtensions);
    return () => {
      document.removeEventListener("visibilitychange", handleVisibility);
      window.removeEventListener("focus", fetchExtensions);
    };
  }, [fetchExtensions]);

  useEffect(() => {
    if (!extensions.some((ext) => ext.status === "loading")) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      void fetchExtensions();
    }, 1500);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [extensions, fetchExtensions]);

  const filtered = useMemo(() => {
    if (!searchTerm) return extensions;
    const q = searchTerm.toLowerCase();
    return extensions.filter((ext) => {
      const name = getDisplayName(ext).toLowerCase();
      return (
        name.includes(q) || (ext.description ?? "").toLowerCase().includes(q)
      );
    });
  }, [extensions, searchTerm]);

  return (
    <Widget
      title={t("contextPanel.widgets.extensions")}
      icon={<IconPuzzle className="size-3.5" />}
      flush
    >
      {extensions.length === 0 ? (
        <p className="px-3 py-2.5 text-xs text-foreground-subtle">
          {t("contextPanel.empty.noExtensions")}
        </p>
      ) : (
        <div>
          <div className="border-b border-border px-3 py-1.5">
            <div className="flex items-center gap-1.5 text-foreground-subtle">
              <IconSearch className="size-3" />
              <Input
                variant="ghost"
                value={searchTerm}
                onChange={(e) => setSearchTerm(e.target.value)}
                placeholder={t("contextPanel.widgets.searchExtensions")}
                className="text-xs"
              />
            </div>
          </div>
          <div className="max-h-40 overflow-y-auto px-3 py-2">
            {filtered.length === 0 ? (
              <p className="py-1 text-xs text-foreground-subtle">
                {t("contextPanel.empty.noMatchingExtensions")}
              </p>
            ) : (
              <div className="space-y-2.5">
                {filtered.map((ext) => (
                  <div key={ext.config_key} className="space-y-1">
                    <div className="flex items-center justify-between gap-2">
                      <span className="min-w-0 truncate text-xs">
                        {getDisplayName(ext)}
                      </span>
                      <ExtensionStatusBadge
                        status={ext.status}
                        label={t(
                          `contextPanel.widgets.extensionStatus.${ext.status}`,
                        )}
                      />
                    </div>
                    {ext.status === "failed" && ext.error && (
                      <p
                        className="truncate text-xs text-danger"
                        title={ext.error}
                      >
                        {ext.error}
                      </p>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}
    </Widget>
  );
}
