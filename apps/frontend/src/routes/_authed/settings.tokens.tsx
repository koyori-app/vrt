import { createFileRoute } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { CopyIcon, PlusIcon } from "lucide-react";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ToneBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { $api, errorMessage, type PersonalToken, type Scope } from "@/lib/api";
import { isExpired, partitionTokens } from "@/lib/personal-tokens";
import { formatDate } from "@/lib/utils";

export const Route = createFileRoute("/_authed/settings/tokens")({
  component: TokensPage,
});

const ALL_SCOPES = [
  { value: "read:project", descriptionKey: "tokens.scopes.readProject" },
  { value: "read:build", descriptionKey: "tokens.scopes.readBuild" },
  { value: "write:build", descriptionKey: "tokens.scopes.writeBuild" },
] as const satisfies readonly { value: Scope; descriptionKey: string }[];

function TokensPage() {
  const { t } = useTranslation();
  const [createOpen, setCreateOpen] = useState(false);
  const queryClient = useQueryClient();
  const tokens = $api.useQuery("get", "/v1/personal_tokens", {});
  const { active, revoked } = partitionTokens(tokens.data);

  const remove = $api.useMutation("delete", "/v1/personal_tokens/{id}", {
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["get", "/v1/personal_tokens"] });
      toast.success(t("tokens.revoked"));
    },
    onError: (error) => toast.error(errorMessage(error, t("tokens.revokeFailed"))),
  });

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">{t("tokens.title")}</h1>
          <p className="text-sm text-muted-foreground">{t("tokens.description")}</p>
        </div>
        <Button onClick={() => setCreateOpen(true)}>
          <PlusIcon className="size-3.5" />
          {t("tokens.new")}
        </Button>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("tokens.activeTitle")}</CardTitle>
        </CardHeader>
        <CardContent>
          <TokenTable
            tokens={active}
            emptyMessage={tokens.isLoading ? t("common.loading") : t("tokens.noActive")}
            onRevoke={(token) => {
              if (!confirm(t("tokens.revokeConfirm", { name: token.name }))) return;
              remove.mutate({ params: { path: { id: token.id } } });
            }}
            revokePending={remove.isPending}
          />
        </CardContent>
      </Card>

      {/* 失効済みは別の表に落とす。同じ表に混ぜると「使えるトークン」を数える
          のに目で選り分ける必要があり、失効の操作も残ってしまう。1 件も無ければ
          カードごと出さない——空の見出しは読む手がかりにならない。 */}
      {revoked.length > 0 ? (
        <Card className="border-dashed">
          <CardHeader>
            <CardTitle className="text-muted-foreground">{t("tokens.revokedTitle")}</CardTitle>
            <CardDescription>{t("tokens.revokedDescription")}</CardDescription>
          </CardHeader>
          <CardContent>
            {/* onRevoke を渡さない = 操作列ごと出ない。失効済みに押せる失効ボタンを
                残さないための形（disabled ではなく不在にする）。 */}
            <TokenTable tokens={revoked} emptyMessage={null} muted />
          </CardContent>
        </Card>
      ) : null}

      <CreateTokenDialog open={createOpen} onOpenChange={setCreateOpen} />
    </div>
  );
}

/**
 * トークンの表。`onRevoke` を渡した表だけが操作列を持つ。
 *
 * 失効済みの表は同じ列構成のまま操作列を落とす——ボタンを `disabled` で残すと
 * 「押せそうに見えるが何も起きない」状態になり、失効済みかどうかの手がかりにも
 * ならない。
 */
function TokenTable({
  tokens,
  emptyMessage,
  onRevoke,
  revokePending,
  muted,
}: {
  tokens: PersonalToken[];
  /** 0 件のときに出す文言。`null` なら行ごと出さない。 */
  emptyMessage: string | null;
  onRevoke?: (token: PersonalToken) => void;
  revokePending?: boolean;
  muted?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const columnCount = onRevoke ? 5 : 4;

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>{t("tokens.columns.name")}</TableHead>
          <TableHead className="w-28">{t("tokens.columns.token")}</TableHead>
          <TableHead>{t("tokens.columns.scopes")}</TableHead>
          <TableHead className="w-48">{t("tokens.columns.lastUsed")}</TableHead>
          {onRevoke ? <TableHead className="w-24" /> : null}
        </TableRow>
      </TableHeader>
      <TableBody>
        {tokens.map((token) => (
          <TableRow key={token.id} className={muted ? "text-muted-foreground" : undefined}>
            <TableCell>
              {token.name}
              {/* 期限切れは失効ではないが、backend の認証は同じく弾く。使えない
                  ことが名前の隣で分かるようにする。 */}
              {isExpired(token) ? (
                <ToneBadge tone="amber" className="ml-2">
                  {t("tokens.expiredBadge")}
                </ToneBadge>
              ) : null}
            </TableCell>
            <TableCell className="font-mono text-xs text-muted-foreground">
              ····{token.token_last_four}
            </TableCell>
            <TableCell>
              <div className="flex flex-wrap gap-1">
                {token.scopes.map((scope) => (
                  <ToneBadge key={scope} tone={muted ? "gray" : "blue"}>
                    {scope}
                  </ToneBadge>
                ))}
              </div>
            </TableCell>
            <TableCell className="text-xs text-muted-foreground">
              {formatDate(token.last_used_at, i18n.language)}
            </TableCell>
            {onRevoke ? (
              <TableCell className="text-right">
                {/* 破壊的な操作なので destructive で出す。ghost のままだと
                    「表示を切り替えるだけ」の操作と見分けが付かない。 */}
                <Button
                  variant="destructive"
                  size="sm"
                  disabled={revokePending}
                  onClick={() => onRevoke(token)}
                >
                  {t("tokens.revoke")}
                </Button>
              </TableCell>
            ) : null}
          </TableRow>
        ))}
        {tokens.length === 0 && emptyMessage !== null ? (
          <TableRow>
            <TableCell colSpan={columnCount} className="text-sm text-muted-foreground">
              {emptyMessage}
            </TableCell>
          </TableRow>
        ) : null}
      </TableBody>
    </Table>
  );
}

function CreateTokenDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<Scope[]>(["read:build", "write:build"]);
  const [issued, setIssued] = useState<string | null>(null);

  const create = $api.useMutation("post", "/v1/personal_tokens", {
    onSuccess: async (token) => {
      await queryClient.invalidateQueries({ queryKey: ["get", "/v1/personal_tokens"] });
      // The raw secret exists only in this response — surface it once.
      setIssued(token.token);
    },
    onError: (error) => toast.error(errorMessage(error, t("tokens.createFailed"))),
  });

  function close() {
    onOpenChange(false);
    setIssued(null);
    setName("");
    setScopes(["read:build", "write:build"]);
    create.reset();
  }

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    create.mutate({ body: { name: name.trim(), scopes, expires_at: null } });
  }

  return (
    <Dialog open={open} onOpenChange={(next) => (next ? onOpenChange(true) : close())}>
      <DialogContent>
        {issued ? (
          <>
            <DialogHeader>
              <DialogTitle>{t("tokens.copyTitle")}</DialogTitle>
              <DialogDescription>{t("tokens.copyDescription")}</DialogDescription>
            </DialogHeader>
            <div className="flex items-center gap-2 py-4">
              <code className="min-w-0 flex-1 truncate rounded-md bg-muted px-3 py-2 font-mono text-xs">
                {issued}
              </code>
              <Button
                variant="outline"
                size="sm"
                onClick={async () => {
                  await navigator.clipboard.writeText(issued);
                  toast.success(t("tokens.copied"));
                }}
              >
                <CopyIcon className="size-3.5" />
                {t("tokens.copy")}
              </Button>
            </div>
            <DialogFooter>
              <Button onClick={close}>{t("tokens.done")}</Button>
            </DialogFooter>
          </>
        ) : (
          <form onSubmit={onSubmit}>
            <DialogHeader>
              <DialogTitle>{t("tokens.createTitle")}</DialogTitle>
              <DialogDescription>{t("tokens.createDescription")}</DialogDescription>
            </DialogHeader>
            <div className="space-y-4 py-4">
              <div className="space-y-2">
                <Label htmlFor="token-name">{t("tokens.columns.name")}</Label>
                <Input
                  id="token-name"
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                  placeholder="github-actions"
                  required
                  autoFocus
                />
              </div>
              <fieldset className="space-y-3">
                <legend className="text-sm font-medium">{t("tokens.columns.scopes")}</legend>
                {ALL_SCOPES.map((scope) => (
                  <label key={scope.value} className="flex items-start gap-3 text-sm">
                    <Checkbox
                      className="mt-0.5"
                      checked={scopes.includes(scope.value)}
                      onCheckedChange={(checked) =>
                        setScopes((current) =>
                          checked
                            ? [...current, scope.value]
                            : current.filter((value) => value !== scope.value),
                        )
                      }
                    />
                    <span>
                      <span className="font-mono text-xs">{scope.value}</span>
                      <span className="block text-xs text-muted-foreground">
                        {t(scope.descriptionKey)}
                      </span>
                    </span>
                  </label>
                ))}
              </fieldset>
            </div>
            <DialogFooter>
              <Button type="button" variant="ghost" onClick={close}>
                {t("common.cancel")}
              </Button>
              <Button type="submit" disabled={create.isPending || !name.trim() || !scopes.length}>
                {create.isPending ? t("tokens.creating") : t("tokens.create")}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
