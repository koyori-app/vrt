import { useNavigate } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
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
import { $api, errorMessage } from "@/lib/api";
import { slugify } from "@/lib/utils";

export function CreateTenantDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugTouched, setSlugTouched] = useState(false);
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const createTenant = $api.useMutation("post", "/v1/tenants", {
    onSuccess: async (tenant) => {
      await queryClient.invalidateQueries({ queryKey: ["get", "/v1/tenants"] });
      onOpenChange(false);
      setName("");
      setSlug("");
      setSlugTouched(false);
      toast.success(t("createTenant.created", { name: tenant.name }));
      await navigate({ to: "/t/$tenantSlug", params: { tenantSlug: tenant.slug } });
    },
    onError: (error) => toast.error(errorMessage(error, t("createTenant.failed"))),
  });

  const effectiveSlug = slugTouched ? slug : slugify(name);

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    createTenant.mutate({
      body: { name: name.trim(), slug: effectiveSlug, avatar_url: null },
    });
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={onSubmit}>
          <DialogHeader>
            <DialogTitle>{t("createTenant.title")}</DialogTitle>
            <DialogDescription>{t("createTenant.description")}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label htmlFor="tenant-name">{t("createTenant.name")}</Label>
              <Input
                id="tenant-name"
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder={t("createTenant.namePlaceholder")}
                required
                autoFocus
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="tenant-slug">{t("createTenant.slug")}</Label>
              <Input
                id="tenant-slug"
                value={effectiveSlug}
                onChange={(event) => {
                  setSlugTouched(true);
                  setSlug(event.target.value);
                }}
                placeholder={t("createTenant.slugPlaceholder")}
                required
              />
              <p className="text-xs text-muted-foreground">
                {t("createTenant.slugHint", { slug: effectiveSlug || "…" })}
              </p>
            </div>
          </div>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={createTenant.isPending || !name || !effectiveSlug}>
              {createTenant.isPending ? t("createTenant.submitting") : t("createTenant.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
