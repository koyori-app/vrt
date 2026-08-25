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
import { $api, errorMessage, type Scope } from "@/lib/api";
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
        <CardContent>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t("tokens.columns.name")}</TableHead>
                <TableHead className="w-28">{t("tokens.columns.token")}</TableHead>
                <TableHead>{t("tokens.columns.scopes")}</TableHead>
                <TableHead className="w-48">{t("tokens.columns.lastUsed")}</TableHead>
                <TableHead className="w-24" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {tokens.data?.map((token) => (
                <TableRow key={token.id}>
                  <TableCell>
                    {token.name}
                    {token.revoked ? (
                      <ToneBadge tone="red" className="ml-2">
                        {t("tokens.revokedBadge")}
                      </ToneBadge>
                    ) : null}
                  </TableCell>
                  <TableCell className="font-mono text-xs text-muted-foreground">
                    ····{token.token_last_four}
                  </TableCell>
                  <TableCell>
                    <div className="flex flex-wrap gap-1">
                      {token.scopes.map((scope) => (
                        <ToneBadge key={scope} tone="blue">
                          {scope}
                        </ToneBadge>
                      ))}
                    </div>
                  </TableCell>
                  <TableCell className="text-xs text-muted-foreground">
                    {formatDate(token.last_used_at)}
                  </TableCell>
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={remove.isPending}
                      onClick={() => {
                        if (!confirm(t("tokens.revokeConfirm", { name: token.name }))) return;
                        remove.mutate({ params: { path: { id: token.id } } });
                      }}
                    >
                      {t("tokens.revoke")}
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
              {!tokens.data?.length ? (
                <TableRow>
                  <TableCell colSpan={5} className="text-sm text-muted-foreground">
                    {tokens.isLoading ? t("common.loading") : t("tokens.empty")}
                  </TableCell>
                </TableRow>
              ) : null}
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <CreateTokenDialog open={createOpen} onOpenChange={setCreateOpen} />
    </div>
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
