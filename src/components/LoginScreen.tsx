import { useState } from "react";
import { httpCommands } from "../lib/transport/http";
import { errorText } from "../lib/errors";

// Rendered by App.tsx when a `convertbar:unauthorized` event fires (server head only).
export default function LoginScreen() {
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setSubmitting(true);
    setError(null);
    try {
      await httpCommands.login(token);
      // Simplest way to re-mount the whole app authenticated: reload and let every
      // page's initial fetch run again against the now-cookied session.
      window.location.reload();
    } catch (err) {
      // Through the shared helper like every other display site: login has no blocking work
      // today, so it cannot produce a panic — but keeping its own extraction here made
      // errors.ts's "single place" claim false and left the trap armed for whoever adds one.
      setError(errorText(err));
      setSubmitting(false);
    }
  };

  return (
    <div className="login-screen">
      <form className="login-form" onSubmit={handleSubmit}>
        <label htmlFor="login-token">Access token</label>
        <input
          id="login-token"
          className="setting-input"
          type="password"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          autoFocus
        />
        <button type="submit" className="btn" disabled={submitting || !token}>
          {submitting ? "Signing in…" : "Sign in"}
        </button>
        {error && <p className="login-error">{error}</p>}
      </form>
    </div>
  );
}
