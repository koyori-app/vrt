import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ToneBadge } from "@/components/status-badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { $api, errorMessage, type TenantRole } from "@/lib/api";
import { roleAtLeast, useMyRole, useResolvedTenant } from "@/lib/queries";
import { formatDate } from "@/lib/utils";

export const Route = createFileRoute("/_authed/t/$tenantSlug/settings")({
  component: TenantSettings,
});

const ROLES: TenantRole[] = ["member", "admin", "owner"];

const roleTone = (role: TenantRole) =>
  role === "owner" ? "blue" : role === "admin" ? "amber" : "gray";

function TenantSettings() {
  const { tenantSlug } = Route.useParams();
  const { me } = Route.useRouteContext();
  const { tenant, isLoading } = useResolvedTenant(tenantSlug);
  const { role } = useMyRole(tenant?.id);

  if (!tenant) {
    return (
      <p className="py-16 text-center text-sm text-muted-foreground">
        {isLoading ? "Loading…" : `No tenant named “${tenantSlug}”.`}
      </p>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">{tenant.name}</h1>
        <p className="text-sm text-muted-foreground">Tenant settings</p>
      </div>

      <MembersCard tenantId={tenant.id} myUserId={me.id} myRole={role} />
      <InstallationsCard tenantId={tenant.id} canManage={roleAtLeast(role, "admin")} />
      {roleAtLeast(role, "owner") ? (
        <DangerZone tenantId={tenant.id} tenantName={tenant.name} />
      ) : null}
    </div>
  );
}

function MembersCard({
  tenantId,
  myUserId,
  myRole,
}: {
  tenantId: string;
  myUserId: string;
  myRole: TenantRole | undefined;
}) {
  const queryClient = useQueryClient();
  const members = $api.useQuery("get", "/v1/tenants/{tenant_id}/members", {
    params: { path: { tenant_id: tenantId } },
  });
  const [username, setUsername] = useState("");
  const [newRole, setNewRole] = useState<TenantRole>("member");
  const canManage = roleAtLeast(myRole, "admin");

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["get", "/v1/tenants/{tenant_id}/members"] });

  const addMember = $api.useMutation("post", "/v1/tenants/{tenant_id}/members", {
    onSuccess: async () => {
      await invalidate();
      setUsername("");
      toast.success("Member added");
    },
    onError: (error) => toast.error(errorMessage(error, "Could not add member")),
  });

  const updateMember = $api.useMutation("patch", "/v1/tenants/{tenant_id}/members/{user_id}", {
    onSuccess: async () => {
      await invalidate();
      toast.success("Role updated");
    },
    onError: (error) => toast.error(errorMessage(error, "Could not update role")),
  });

  const removeMember = $api.useMutation("delete", "/v1/tenants/{tenant_id}/members/{user_id}", {
    onSuccess: async () => {
      await invalidate();
      toast.success("Member removed");
    },
    onError: (error) => toast.error(errorMessage(error, "Could not remove member")),
  });

  function onAdd(event: FormEvent) {
    event.preventDefault();
    addMember.mutate({
      params: { path: { tenant_id: tenantId } },
      body: { username: username.trim(), user_id: null, role: newRole },
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Members</CardTitle>
        <CardDescription>Owners and admins can invite people and change roles.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>User</TableHead>
              <TableHead>Role</TableHead>
              <TableHead>Joined</TableHead>
              <TableHead className="w-24" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {members.data?.map((member) => (
              <TableRow key={member.id}>
                <TableCell>
                  <div className="flex items-center gap-2">
                    {member.avatar_url ? (
                      <img
                        src={member.avatar_url}
                        alt=""
                        className="size-6 shrink-0 rounded-full object-cover"
                      />
                    ) : null}
                    <div className="min-w-0">
                      <div className="truncate text-sm font-medium">
                        {member.display_name || member.username || member.user_id}
                        {member.user_id === myUserId ? (
                          <span className="ml-2 font-normal text-muted-foreground">(you)</span>
                        ) : null}
                      </div>
                      {member.username ? (
                        <div className="truncate text-xs text-muted-foreground">
                          @{member.username}
                        </div>
                      ) : null}
                    </div>
                  </div>
                </TableCell>
                <TableCell>
                  {canManage && member.user_id !== myUserId ? (
                    <Select
                      value={member.role}
                      disabled={updateMember.isPending}
                      onValueChange={(value) =>
                        updateMember.mutate({
                          params: { path: { tenant_id: tenantId, user_id: member.user_id } },
                          body: { role: value as TenantRole },
                        })
                      }
                    >
                      <SelectTrigger size="sm" className="w-32">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {ROLES.map((role) => (
                          <SelectItem key={role} value={role}>
                            {role}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  ) : (
                    <ToneBadge tone={roleTone(member.role)}>{member.role}</ToneBadge>
                  )}
                </TableCell>
                <TableCell className="text-xs text-muted-foreground">
                  {formatDate(member.created_at)}
                </TableCell>
                <TableCell className="text-right">
                  {canManage && member.user_id !== myUserId ? (
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={removeMember.isPending}
                      onClick={() => {
                        if (!confirm("Remove this member from the tenant?")) return;
                        removeMember.mutate({
                          params: { path: { tenant_id: tenantId, user_id: member.user_id } },
                        });
                      }}
                    >
                      Remove
                    </Button>
                  ) : null}
                </TableCell>
              </TableRow>
            ))}
            {!members.data?.length ? (
              <TableRow>
                <TableCell colSpan={4} className="text-sm text-muted-foreground">
                  {members.isLoading ? "Loading…" : "No members."}
                </TableCell>
              </TableRow>
            ) : null}
          </TableBody>
        </Table>

        {canManage ? (
          <form onSubmit={onAdd} className="flex flex-wrap items-end gap-2">
            <div className="space-y-2">
              <Label htmlFor="member-username">Add by username</Label>
              <Input
                id="member-username"
                className="w-56"
                value={username}
                onChange={(event) => setUsername(event.target.value)}
                placeholder="octocat"
                required
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="member-role">Role</Label>
              <Select value={newRole} onValueChange={(value) => setNewRole(value as TenantRole)}>
                <SelectTrigger id="member-role" className="w-32">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {ROLES.map((role) => (
                    <SelectItem key={role} value={role}>
                      {role}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <Button type="submit" disabled={addMember.isPending || !username.trim()}>
              {addMember.isPending ? "Adding…" : "Add member"}
            </Button>
          </form>
        ) : null}
      </CardContent>
    </Card>
  );
}

function InstallationsCard({ tenantId, canManage }: { tenantId: string; canManage: boolean }) {
  const queryClient = useQueryClient();
  const claimed = $api.useQuery("get", "/v1/github/installations", {
    params: { query: { tenant_id: tenantId } },
  });
  const unclaimed = $api.useQuery("get", "/v1/github/installations/unclaimed", {});

  const claim = $api.useMutation("post", "/v1/github/installations/{installation_id}/claim", {
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["get", "/v1/github/installations"] }),
        queryClient.invalidateQueries({
          queryKey: ["get", "/v1/github/installations/unclaimed"],
        }),
      ]);
      toast.success("Installation linked to this tenant");
    },
    onError: (error) => toast.error(errorMessage(error, "Could not claim installation")),
  });

  return (
    <Card>
      <CardHeader>
        <CardTitle>GitHub installations</CardTitle>
        <CardDescription>
          Link a GitHub App installation to post commit statuses back to pull requests.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        <div className="space-y-2">
          <h3 className="text-sm font-medium">Linked</h3>
          {claimed.data?.installations.length ? (
            <ul className="divide-y divide-border rounded-md border border-border">
              {claimed.data.installations.map((installation) => (
                <li
                  key={installation.id}
                  className="flex items-center justify-between gap-3 px-3 py-2 text-sm"
                >
                  <span>
                    {installation.account_login}
                    <span className="ml-2 text-xs text-muted-foreground">
                      #{installation.installation_id} · {installation.account_type}
                    </span>
                  </span>
                  {installation.suspended ? <ToneBadge tone="red">Suspended</ToneBadge> : null}
                </li>
              ))}
            </ul>
          ) : (
            <p className="text-sm text-muted-foreground">
              {claimed.isLoading ? "Loading…" : "No installations linked yet."}
            </p>
          )}
        </div>

        <div className="space-y-2">
          <h3 className="text-sm font-medium">Unclaimed</h3>
          {unclaimed.data?.installations.length ? (
            <ul className="divide-y divide-border rounded-md border border-border">
              {unclaimed.data.installations.map((installation) => (
                <li
                  key={installation.id}
                  className="flex items-center justify-between gap-3 px-3 py-2 text-sm"
                >
                  <span>
                    {installation.account_login}
                    <span className="ml-2 text-xs text-muted-foreground">
                      #{installation.installation_id}
                    </span>
                  </span>
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={!canManage || claim.isPending}
                    onClick={() =>
                      claim.mutate({
                        params: { path: { installation_id: installation.installation_id } },
                        body: { tenant_id: tenantId },
                      })
                    }
                  >
                    Claim
                  </Button>
                </li>
              ))}
            </ul>
          ) : (
            <p className="text-sm text-muted-foreground">
              {unclaimed.isLoading ? "Loading…" : "Nothing waiting to be claimed."}
            </p>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

function DangerZone({ tenantId, tenantName }: { tenantId: string; tenantName: string }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const deleteTenant = $api.useMutation("delete", "/v1/tenants/{tenant_id}", {
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["get", "/v1/tenants"] });
      toast.success("Tenant deleted");
      await navigate({ to: "/" });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not delete tenant")),
  });

  return (
    <Card className="border-destructive/40">
      <CardHeader>
        <CardTitle className="text-destructive">Danger zone</CardTitle>
        <CardDescription>
          Deleting a tenant removes its projects, builds and stored screenshots. This cannot be
          undone.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <Button
          variant="destructive"
          disabled={deleteTenant.isPending}
          onClick={() => {
            const answer = prompt(`Type “${tenantName}” to confirm deletion.`);
            if (answer !== tenantName) return;
            deleteTenant.mutate({ params: { path: { tenant_id: tenantId } } });
          }}
        >
          Delete tenant
        </Button>
      </CardContent>
    </Card>
  );
}
