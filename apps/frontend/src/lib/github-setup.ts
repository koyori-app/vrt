/**
 * GitHub App インストール導線の戻り先を、このタブの sessionStorage で受け渡す。
 *
 * GitHub の setup callback には任意の `state` を付けて他人に踏ませられるため、
 * 戻り先パスを `state` にそのまま載せるとオープンリダイレクト相当の誘導に使われる。
 * `state` はサーバが発行した不透明な one-time トークンだけに使い、
 * 戻り先はインストールを開始したタブ自身が覚えておく。
 */

const KEY_PREFIX = "vrt:github-setup-return:";

/** インストール開始時に、この state に対応する戻り先パスを記録する。 */
export function rememberSetupReturnPath(state: string, path: string) {
  try {
    window.sessionStorage.setItem(KEY_PREFIX + state, path);
  } catch {
    // sessionStorage が使えない環境では戻り先を "/" にフォールバックするだけ。
  }
}

/** callback 後の戻り先を取り出す。記録が無ければ "/"。取り出したら消す。 */
export function readSetupReturnPath(state: string | undefined): string {
  if (!state) return "/";
  let stored: string | null = null;
  try {
    stored = window.sessionStorage.getItem(KEY_PREFIX + state);
    window.sessionStorage.removeItem(KEY_PREFIX + state);
  } catch {
    stored = null;
  }
  // 自分で書いた値だが、念のため同一オリジンの絶対パスであることを確認する。
  if (stored?.startsWith("/") && !stored.startsWith("//")) return stored;
  return "/";
}
