import { $api, type Build, type Project, type Tenant, type TenantRole } from "@/lib/api";

/** `/users/me` is the auth source of truth; keep it warm but not stale. */
export const ME_STALE_TIME_MS = 60_000;
/** Tenants/projects change rarely and every route resolves slugs through them. */
export const DIRECTORY_STALE_TIME_MS = 30_000;

export const meQueryOptions = () =>
  $api.queryOptions("get", "/v1/users/me", {}, { staleTime: ME_STALE_TIME_MS, retry: false });

export const tenantsQueryOptions = () =>
  $api.queryOptions("get", "/v1/tenants", {}, { staleTime: DIRECTORY_STALE_TIME_MS });

export const projectsQueryOptions = (tenantId: string) =>
  $api.queryOptions(
    "get",
    "/v1/tenants/{tenant_id}/projects",
    { params: { path: { tenant_id: tenantId } } },
    { staleTime: DIRECTORY_STALE_TIME_MS },
  );

export function useMe() {
  return $api.useQuery("get", "/v1/users/me", {}, { staleTime: ME_STALE_TIME_MS, retry: false });
}

export function useTenants() {
  return $api.useQuery("get", "/v1/tenants", {}, { staleTime: DIRECTORY_STALE_TIME_MS });
}

/**
 * Backend routes are id-based while the URLs are slug-based, so every
 * `/t/$tenantSlug` screen resolves the slug against the tenant list first
 * (same idea as task's `useResolvedTenantId`).
 */
export function useResolvedTenant(slug: string) {
  const query = useTenants();
  const tenant: Tenant | undefined = query.data?.find((t) => t.slug === slug);
  return { ...query, tenant, tenantId: tenant?.id };
}

export function useProjects(tenantId: string | undefined) {
  return $api.useQuery(
    "get",
    "/v1/tenants/{tenant_id}/projects",
    { params: { path: { tenant_id: tenantId ?? "" } } },
    { enabled: !!tenantId, staleTime: DIRECTORY_STALE_TIME_MS },
  );
}

export function useResolvedProject(tenantId: string | undefined, projectSlug: string) {
  const query = useProjects(tenantId);
  const project: Project | undefined = query.data?.find((p) => p.slug === projectSlug);
  return { ...query, project, projectId: project?.id };
}

const ROLE_RANK: Record<TenantRole, number> = { member: 0, admin: 1, owner: 2 };

export function roleAtLeast(role: TenantRole | undefined, minimum: TenantRole) {
  if (!role) return false;
  return ROLE_RANK[role] >= ROLE_RANK[minimum];
}

/**
 * The caller's role rides along on the tenant list (`my_role`), so no extra
 * request is needed — every route already loads the tenant list to resolve slugs.
 */
export function useMyRole(tenantId: string | undefined) {
  const tenants = useTenants();
  const role = tenants.data?.find((t) => t.id === tenantId)?.my_role ?? undefined;
  return { role, isLoading: tenants.isLoading };
}

export function useBuilds(projectId: string | undefined, limit = 50) {
  return $api.useQuery(
    "get",
    "/v1/projects/{project_id}/builds",
    { params: { path: { project_id: projectId ?? "" }, query: { limit } } },
    { enabled: !!projectId },
  );
}

/** Latest build first — the list endpoint already returns newest-first. */
export function latestBuild(builds: Build[] | undefined): Build | undefined {
  return builds?.[0];
}
