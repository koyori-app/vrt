import { createFileRoute } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { CopyIcon, PlusIcon } from "lucide-react";
import { useState, type FormEvent } from "react";
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

const ALL_SCOPES: { value: Scope; description: string }[] = [
  { value: "read:project", description: "Read projects and their settings" },
  { value: "read:build", description: "Read builds and comparison results" },
  { value: "write:build", description: "Create builds, upload screenshots, finalize" },
];

function TokensPage() {
  const [createOpen, setCreateOpen] = useState(false);
  const queryClient = useQueryClient();
  const tokens = $api.useQuery("get", "/v1/personal_tokens", {});

  const remove = $api.useMutation("delete", "/v1/personal_tokens/{id}", {
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["get", "/v1/personal_tokens"] });
      toast.success("Token revoked");
    },
    onError: (error) => toast.error(errorMessage(error, "Could not revoke token")),
  });

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Personal access tokens</h1>
          <p className="text-sm text-muted-foreground">
            Used by CI to create builds and upload screenshots.
          </p>
        </div>
        <Button onClick={() => setCreateOpen(true)}>
          <PlusIcon className="size-3.5" />
          New token
        </Button>
      </div>

      <Card>
        <CardContent>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead className="w-28">Token</TableHead>
                <TableHead>Scopes</TableHead>
                <TableHead className="w-48">Last used</TableHead>
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
                        Revoked
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
                        if (!confirm(`Revoke “${token.name}”? CI using it will start failing.`))
                          return;
                        remove.mutate({ params: { path: { id: token.id } } });
                      }}
                    >
                      Revoke
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
              {!tokens.data?.length ? (
                <TableRow>
                  <TableCell colSpan={5} className="text-sm text-muted-foreground">
                    {tokens.isLoading ? "Loading…" : "No tokens yet."}
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
    onError: (error) => toast.error(errorMessage(error, "Could not create token")),
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
              <DialogTitle>Copy your token</DialogTitle>
              <DialogDescription>
                This is the only time the token is shown. Store it in your CI secrets now.
              </DialogDescription>
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
                  toast.success("Token copied");
                }}
              >
                <CopyIcon className="size-3.5" />
                Copy
              </Button>
            </div>
            <DialogFooter>
              <Button onClick={close}>Done</Button>
            </DialogFooter>
          </>
        ) : (
          <form onSubmit={onSubmit}>
            <DialogHeader>
              <DialogTitle>New personal access token</DialogTitle>
              <DialogDescription>Grant only the scopes your CI job needs.</DialogDescription>
            </DialogHeader>
            <div className="space-y-4 py-4">
              <div className="space-y-2">
                <Label htmlFor="token-name">Name</Label>
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
                <legend className="text-sm font-medium">Scopes</legend>
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
                        {scope.description}
                      </span>
                    </span>
                  </label>
                ))}
              </fieldset>
            </div>
            <DialogFooter>
              <Button type="button" variant="ghost" onClick={close}>
                Cancel
              </Button>
              <Button type="submit" disabled={create.isPending || !name.trim() || !scopes.length}>
                {create.isPending ? "Creating…" : "Create token"}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
