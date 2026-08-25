import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import {
  CheckIcon,
  ChevronsUpDownIcon,
  KeyIcon,
  LanguagesIcon,
  LogOutIcon,
  PlusIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { $api, errorMessage, type Me } from "@/lib/api";
import { useTenants } from "@/lib/queries";
import { cn } from "@/lib/utils";

export function TopNav({ me, onCreateTenant }: { me: Me; onCreateTenant?: () => void }) {
  const { t } = useTranslation();
  const tenants = useTenants();
  const params = useParams({ strict: false }) as { tenantSlug?: string };
  const activeTenant = tenants.data?.find((t) => t.slug === params.tenantSlug);

  return (
    <header className="sticky top-0 z-30 border-b border-border bg-background/80 backdrop-blur">
      <div className="mx-auto flex h-14 max-w-7xl items-center gap-3 px-4">
        <Link to="/" className="flex items-center gap-2 font-semibold tracking-tight">
          <span className="grid size-6 place-items-center rounded bg-primary text-xs text-primary-foreground">
            V
          </span>
          VRT
        </Link>

        <span className="text-muted-foreground">/</span>

        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="sm" className="gap-1.5">
              {activeTenant?.name ?? t("nav.selectTenant")}
              <ChevronsUpDownIcon className="size-3.5 opacity-60" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-56">
            <DropdownMenuLabel>{t("nav.tenants")}</DropdownMenuLabel>
            {tenants.data?.length ? (
              tenants.data.map((tenant) => (
                <DropdownMenuItem key={tenant.id} asChild>
                  <Link
                    to="/t/$tenantSlug"
                    params={{ tenantSlug: tenant.slug }}
                    className="flex items-center justify-between"
                  >
                    <span className="truncate">{tenant.name}</span>
                    <CheckIcon
                      className={cn(
                        "size-3.5",
                        tenant.id === activeTenant?.id ? "opacity-100" : "opacity-0",
                      )}
                    />
                  </Link>
                </DropdownMenuItem>
              ))
            ) : (
              <DropdownMenuItem disabled>{t("nav.noTenants")}</DropdownMenuItem>
            )}
            {onCreateTenant ? (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem onSelect={() => onCreateTenant()}>
                  <PlusIcon className="size-3.5" />
                  {t("nav.newTenant")}
                </DropdownMenuItem>
              </>
            ) : null}
          </DropdownMenuContent>
        </DropdownMenu>

        <div className="flex-1" />

        <UserMenu me={me} />
      </div>
    </header>
  );
}

function UserMenu({ me }: { me: Me }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const logout = $api.useMutation("post", "/v1/auth/logout", {
    onSuccess: async () => {
      // Drop every cached authenticated response before leaving the layout.
      queryClient.clear();
      await navigate({ to: "/login", search: {} });
    },
    onError: (error) => toast.error(errorMessage(error, t("nav.signOutFailed"))),
  });

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="sm" className="gap-2 pl-1.5">
          <Avatar me={me} />
          <span className="max-w-32 truncate">{me.display_name || me.username}</span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-56">
        <DropdownMenuLabel className="font-normal">
          <div className="truncate font-medium">{me.display_name || me.username}</div>
          <div className="truncate text-xs text-muted-foreground">@{me.username}</div>
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem asChild>
          <Link to="/settings/tokens">
            <KeyIcon className="size-3.5" />
            {t("nav.apiTokens")}
          </Link>
        </DropdownMenuItem>
        <DropdownMenuItem asChild>
          <Link to="/settings/language">
            <LanguagesIcon className="size-3.5" />
            {t("nav.language")}
          </Link>
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem
          disabled={logout.isPending}
          onSelect={(event) => {
            event.preventDefault();
            logout.mutate({});
          }}
        >
          <LogOutIcon className="size-3.5" />
          {t("nav.signOut")}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function Avatar({ me }: { me: Me }) {
  if (me.avatar_url) {
    return (
      <img
        src={me.avatar_url}
        alt=""
        className="size-6 rounded-full object-cover"
        referrerPolicy="no-referrer"
      />
    );
  }
  return (
    <span className="grid size-6 place-items-center rounded-full bg-muted text-[10px] uppercase">
      {(me.username || "?").slice(0, 2)}
    </span>
  );
}
