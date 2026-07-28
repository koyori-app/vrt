import { useNavigate } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
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
      toast.success(`Created ${tenant.name}`);
      await navigate({ to: "/t/$tenantSlug", params: { tenantSlug: tenant.slug } });
    },
    onError: (error) => toast.error(errorMessage(error, "Could not create tenant")),
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
            <DialogTitle>New tenant</DialogTitle>
            <DialogDescription>
              A tenant groups projects, members and GitHub installations.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label htmlFor="tenant-name">Name</Label>
              <Input
                id="tenant-name"
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder="Acme Inc"
                required
                autoFocus
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="tenant-slug">Slug</Label>
              <Input
                id="tenant-slug"
                value={effectiveSlug}
                onChange={(event) => {
                  setSlugTouched(true);
                  setSlug(event.target.value);
                }}
                placeholder="acme"
                required
              />
              <p className="text-xs text-muted-foreground">
                Used in URLs: /t/{effectiveSlug || "…"}
              </p>
            </div>
          </div>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={createTenant.isPending || !name || !effectiveSlug}>
              {createTenant.isPending ? "Creating…" : "Create tenant"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
