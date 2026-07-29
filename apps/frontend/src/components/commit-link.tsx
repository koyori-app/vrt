import { cn, shortSha } from "@/lib/utils";

interface CommitLinkProps {
  /** `owner/repo` as stored on the project, or a full URL / nullish when unset. */
  githubRepo: string | null | undefined;
  commitSha: string;
  className?: string;
}

/**
 * Renders a commit SHA. When the project has a `github_repo`, the shortened SHA
 * links to the commit page on GitHub; otherwise it stays plain text so nothing
 * changes for projects without a linked repo.
 */
export function CommitLink({ githubRepo, commitSha, className }: CommitLinkProps) {
  const label = shortSha(commitSha);

  if (!githubRepo) {
    return <span className={className}>{label}</span>;
  }

  // Normally `owner/repo`, but guard against a full URL being stored so we never
  // build `https://github.com/https://...`.
  const base = githubRepo.startsWith("http") ? githubRepo : `https://github.com/${githubRepo}`;

  return (
    <a
      href={`${base}/commit/${commitSha}`}
      target="_blank"
      rel="noopener noreferrer"
      // The build list rows navigate on click; don't let the link steal that.
      onClick={(e) => e.stopPropagation()}
      className={cn("underline-offset-4 hover:underline", className)}
    >
      {label}
    </a>
  );
}
