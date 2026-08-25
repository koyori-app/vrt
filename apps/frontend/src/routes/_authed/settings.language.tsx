import { createFileRoute } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { $api, errorMessage } from "@/lib/api";
import {
  DEFAULT_LANGUAGE,
  LANGUAGE_LABELS,
  SUPPORTED_LANGUAGES,
  isLanguage,
  normalizeLanguage,
  resolveLanguage,
  type Language,
} from "@/lib/i18n";
import { detectBrowserLanguage } from "@/lib/i18n/detect";
import { useMe } from "@/lib/queries";

export const Route = createFileRoute("/_authed/settings/language")({
  component: LanguagePage,
});

/** 「ブラウザに従う」を表す Select の値。`null` は Radix の Select に渡せない。 */
const AUTO = "auto";

function LanguagePage() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const me = useMe();

  const current = me.data?.language;
  const selected: string = isLanguage(current) ? current : AUTO;

  const update = $api.useMutation("patch", "/v1/users/me", {
    onSuccess: async (user) => {
      // 保存された設定でそのまま描き直す。`/me` は他画面（ナビ等）も読むので
      // キャッシュを捨てて取り直す。
      await queryClient.invalidateQueries({ queryKey: ["get", "/v1/users/me"] });
      await i18n.changeLanguage(resolveLanguage(user.language, detectBrowserLanguage()));
      toast.success(t("language.saved"));
    },
    onError: (error) => toast.error(errorMessage(error, t("language.failed"))),
  });

  function onSelect(value: string) {
    const language: Language | null = isLanguage(value) ? value : null;
    update.mutate({ body: { language } });
  }

  return (
    <div className="mx-auto max-w-lg space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">{t("language.title")}</h1>
        <p className="text-sm text-muted-foreground">{t("language.description")}</p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("language.field")}</CardTitle>
          <CardDescription>
            {t("language.autoHint", {
              language: LANGUAGE_LABELS[normalizeLanguage(i18n.language) ?? DEFAULT_LANGUAGE],
            })}
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-2">
          <Label htmlFor="language">{t("language.field")}</Label>
          <Select value={selected} onValueChange={onSelect} disabled={update.isPending}>
            <SelectTrigger id="language" className="w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={AUTO}>{t("language.auto")}</SelectItem>
              {SUPPORTED_LANGUAGES.map((language) => (
                <SelectItem key={language} value={language}>
                  {LANGUAGE_LABELS[language]}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </CardContent>
      </Card>
    </div>
  );
}
