import { useEffect, useState } from "react";

import { useAppState } from "@/app/AppState";
import { useTranslate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import { Button, Card, Choice, Field, Input, PageHeader, Toggle } from "@/shared/ui";
import type { AppMode } from "@/shared/types/AppMode";

import { DiagnosticsPanel } from "./DiagnosticsPanel";

export function SettingsScreen() {
  const t = useTranslate();
  const { settings, patchSettings, reportFailure } = useAppState();

  if (!settings) {
    return <p className="p-6 text-sm text-ink-faint">{t("common.loading")}</p>;
  }

  return (
    <div className="mx-auto max-w-3xl space-y-5 px-8 py-8">
      <PageHeader title={t("settings.title")} />

      <Card title={t("settings.general")}>
        <div className="space-y-4">
          <Field label={t("settings.nickname")} hint={t("settings.nickname.hint")}>
            <Input
              defaultValue={settings.nickname}
              onBlur={(event) => {
                const nickname = event.target.value.trim();
                if (nickname && nickname !== settings.nickname) {
                  void patchSettings({ nickname }).catch(reportFailure);
                }
              }}
            />
          </Field>

          <div className="grid gap-4 sm:grid-cols-2">
            <Field label={t("settings.room")}>
              <Input
                defaultValue={settings.room}
                onBlur={(event) => {
                  const room = event.target.value.trim();
                  if (room && room !== settings.room) {
                    void patchSettings({ room }).catch(reportFailure);
                  }
                }}
              />
            </Field>

            <Field label={t("settings.port")}>
              <Input
                type="number"
                min={1024}
                max={65535}
                defaultValue={settings.port}
                onBlur={(event) => {
                  const port = Number(event.target.value);
                  // Anything below 1024 needs elevation on both platforms.
                  if (port >= 1024 && port <= 65535 && port !== settings.port) {
                    void patchSettings({ port }).catch(reportFailure);
                  }
                }}
              />
            </Field>
          </div>

          <div className="grid gap-4 sm:grid-cols-2">
            <Choice
              label={t("settings.language")}
              value={settings.language}
              options={[
                { value: "en", label: "English" },
                { value: "tr", label: "Türkçe" },
              ]}
              onChange={(language) =>
                void patchSettings({ language }).catch(reportFailure)
              }
            />

            <Choice
              label={t("settings.mode")}
              value={settings.mode ?? "host"}
              options={[
                { value: "host", label: t("mode.host") },
                { value: "guest", label: t("mode.guest") },
              ]}
              onChange={(mode) =>
                void patchSettings({ mode: mode as AppMode }).catch(reportFailure)
              }
            />
          </div>
        </div>
      </Card>

      <Card title={t("settings.monitor")}>
        <Toggle
          checked={settings.monitorEnabled}
          label={t("settings.monitor")}
          hint={t("settings.monitor.hint")}
          onChange={(monitorEnabled) =>
            void patchSettings({ monitorEnabled }).catch(reportFailure)
          }
        />
      </Card>

      {/* ponytail: `skipSetupWhenReady` no longer has a screen to skip — the
          setup check lives in the launch strip now. The backend field stays
          until settings change for some other reason. */}

      <DiagnosticsPanel />

      <DiscordSettings
        enabled={settings.discordEnabled}
        onToggle={(discordEnabled) =>
          void patchSettings({ discordEnabled }).catch(reportFailure)
        }
      />
    </div>
  );
}

function DiscordSettings({
  enabled,
  onToggle,
}: {
  enabled: boolean;
  onToggle: (next: boolean) => void;
}) {
  const t = useTranslate();

  const [configured, setConfigured] = useState(false);
  const [url, setUrl] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    void ipc.discordStatus().then(setConfigured);
  }, []);

  async function attempt(action: () => Promise<unknown>, success: string) {
    setNotice(null);
    setProblem(null);
    try {
      await action();
      setConfigured(await ipc.discordStatus());
      setNotice(success);
    } catch (error) {
      setProblem(errorMessage(error));
    }
  }

  return (
    <Card title={t("settings.discord")}>
      <div className="space-y-4">
        <Toggle
          checked={enabled}
          label={t("settings.discord.enable")}
          onChange={onToggle}
        />

        <Field label={t("settings.discord.webhook")}>
          <Input
            type="url"
            value={url}
            placeholder={t("settings.discord.webhook.placeholder")}
            onChange={(event) => setUrl(event.target.value)}
          />
        </Field>

        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="primary"
            disabled={!url.trim()}
            onClick={() =>
              void attempt(async () => {
                await ipc.setDiscordWebhook(url);
                setUrl("");
              }, t("settings.saved"))
            }
          >
            {t("common.save")}
          </Button>

          <Button
            disabled={!configured}
            onClick={() =>
              void attempt(ipc.testDiscordWebhook, t("settings.discord.sent"))
            }
          >
            {t("settings.discord.test")}
          </Button>

          <Button
            variant="ghost"
            disabled={!configured}
            onClick={() =>
              void attempt(ipc.clearDiscordWebhook, t("settings.saved"))
            }
          >
            {t("settings.discord.clear")}
          </Button>
        </div>

        <p className="text-xs text-ink-faint">
          {configured
            ? t("settings.discord.configured")
            : t("settings.discord.notConfigured")}
        </p>

        {notice && <p className="text-sm text-good">{notice}</p>}
        {problem && <p className="text-sm text-bad">{problem}</p>}
      </div>
    </Card>
  );
}
