# macOS Code Signing & Notarization — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.
>
> **Tasks 1–3 are human-only.** They happen in a browser (developer.apple.com, App Store Connect) and in Keychain Access, and they handle secret material that must never reach a transcript. An agent executing this plan runs Tasks 4–6 and *verifies* Tasks 1–3 by their observable outputs.

**Goal:** Ship Developer ID–signed, notarized macOS builds from GitHub Actions so TCC permission grants survive rebuilds and Gatekeeper stops blocking downloaded builds.

**Architecture:** The signing certificate is created from a Keychain Access CSR (no Xcode) and reaches CI as a base64 `.p12` secret. Notarization authenticates with an **App Store Connect API key** (`.p8`), never an Apple ID or app-specific password. Signing is **CI-only** — `tauri.conf.json` gains no `signingIdentity`, so `scripts/release.sh`'s local rebuild keeps working exactly as it does today.

**Tech Stack:** Tauri 2 bundler (`tauri-apps/tauri-action`), `codesign`, `notarytool`, `stapler` — all from Command Line Tools.

## Global Constraints

- **No Xcode.** Command Line Tools only. Verified on the target machine: `xcode-select -p` → `/Library/Developer/CommandLineTools`, and both `notarytool` and `stapler` resolve inside it. Every command in this plan is CLT-only.
- **No Apple ID and no app-specific password, anywhere** — not in GitHub secrets, not in a local shell, not in a transcript. Notarization uses `APPLE_API_ISSUER` + `APPLE_API_KEY` + `APPLE_API_KEY_PATH` exclusively. If a step ever asks for `APPLE_ID`/`APPLE_PASSWORD`, that step is wrong.
- The certificate must be **Developer ID Application**. Not "Apple Development", not "Apple Distribution", not "Mac Developer". Those are for Xcode-managed and App Store flows and will not pass Gatekeeper for direct distribution. (The Tauri docs' CI example greps for `Apple Development` — that example is for a different distribution channel; do not copy it.)
- **`src-tauri/tauri.conf.json` gets no `signingIdentity` and no `macOS` block.** Adding one makes every local `npm run tauri build` — including the rebuild inside `scripts/release.sh` — fail on a machine without the private key.
- **The bundle identifier stays `com.convertbar.app`.** It is the anchor for both the TCC grants this work is meant to stabilize and the updater's app identity.
- **No entitlements file.** Under the hardened runtime ConvertBar needs none: it spawns HandBrakeCLI as a *separate process* (not a loaded library), it is not sandboxed so file access is unrestricted, and its webview's JIT lives in Apple's own XPC processes. Checked against the actual sources — no `dlopen`, `libloading`, `DYLD_*` manipulation, `fork` without `exec`, or unsigned dylibs anywhere. Add an entitlement only in response to an observed failure, never speculatively. The hardened runtime itself needs no configuration: `hardenedRuntime` defaults to `true` in the installed Tauri CLI's config schema.
- **Never commit `.p12`, `.p8`, `.cer`, or `.certSigningRequest` files.** Task 6 adds the `.gitignore` guard; until then, keep them on the Desktop and delete them after upload.
- The `.p8` private key is **downloadable exactly once**. Put it in 1Password before closing the browser tab.
- `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in `.github/workflows/build.yml:92-93` are the **updater** minisign keys. They are unrelated to code signing. Do not touch, rename, or reuse them.
- **This change costs the current TCC grants one last time.** Moving from ad-hoc to Developer ID is itself a signature change, so the installed app loses its permissions once more on the first signed release. After that they persist across versions, which is the entire point.
- Commits are signed. On a 1Password agent error, unlock and retry once; if it still fails, report BLOCKED rather than committing unsigned.
- Baseline before this plan: `main` at `ed2573f`, clean tree, no signing configuration anywhere in the repo.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `.github/workflows/build.yml` | Release build matrix | Modify: credential preflight, `.p8` materialization, signing env, post-build verification |
| `.gitignore` | Keep secret material out of the repo | Modify: add cert/key patterns |
| `CLAUDE.md` | Operator documentation | Modify: add a "Code Signing (macOS)" section |
| `src-tauri/tauri.conf.json` | Bundle config | **Unchanged** — deliberately (see Global Constraints) |

---

### Task 1: Create the Developer ID Application certificate

**Human task.** Produces two secret values and one identity string. No Xcode involved — Keychain Access is part of macOS.

**Prerequisites:**
- An **active paid** Apple Developer Program membership. Confirmed by the user; verify in 10 seconds at <https://developer.apple.com/account> — the Membership panel must show an active enrollment. A free Apple ID developer account cannot issue Developer ID certificates, and the portal simply will not offer the option in Step 3.
- Developer ID certificates can only be created by the Account Holder. On an individual membership you are the Account Holder, so this is satisfied.

- [ ] **Step 1: Confirm the starting state**

```bash
security find-identity -v -p codesigning
```

Expected right now: `0 valid identities found`. This is the "test" for this task — it must go from zero identities to exactly one Developer ID Application identity.

- [ ] **Step 2: Generate a Certificate Signing Request**

Open **Keychain Access** (`/System/Applications/Utilities/Keychain Access.app`) → menu **Keychain Access → Certificate Assistant → Request a Certificate From a Certificate Authority…**

- User Email Address: `rouven@rhurling.de`
- Common Name: `Rouven Hurling` (this becomes the private key's label in the login keychain)
- CA Email Address: **leave empty**
- Request is: **Saved to disk**, and tick **Let me specify key pair information**
- Next → Key Size **2048 bits**, Algorithm **RSA**
- Save as `~/Desktop/ConvertBar.certSigningRequest`

This writes the CSR to disk *and* creates the matching private key in your login keychain. The private key never leaves your machine during this step — that pairing is what makes the downloaded certificate usable.

- [ ] **Step 3: Issue the certificate in the developer portal**

Go to <https://developer.apple.com/account/resources/certificates/list> → **+** → under **Software**, select **Developer ID Application** → Continue.

The portal's exact follow-up prompts are not pinned down here (Apple changes them); these two are *guesses at what may appear*, not documented steps — if neither shows up, nothing is wrong:
- If asked to choose a profile type, pick the direct-distribution option (outside the Mac App Store).
- If asked to choose a Sub-CA, pick **G2 Sub-CA** — the default. The "Previous Sub-CA" option exists only for compatibility with very old macOS releases.
- Upload `~/Desktop/ConvertBar.certSigningRequest` → Continue → **Download**.

Note: Apple caps how many Developer ID Application certificates an account may hold (historically 5). Do not create spares.

- [ ] **Step 4: Install the certificate and verify the identity exists**

Double-click the downloaded `developerID_application.cer`. It installs into the login keychain and pairs with the private key from Step 2.

```bash
security find-identity -v -p codesigning
```

Expected: exactly one line, of the form

```
  1) 0123456789ABCDEF... "Developer ID Application: Rouven Hurling (ABCDE12345)"
     1 valid identities found
```

**If it says `Apple Development` instead of `Developer ID Application`, the wrong certificate type was issued — go back to Step 3.** The value inside the quotes, verbatim including the team ID in parentheses, is `APPLE_SIGNING_IDENTITY`.

- [ ] **Step 5: Confirm the certificate's validity window**

```bash
security find-certificate -c "Developer ID Application" -p | openssl x509 -noout -subject -dates
```

Expected: a `notAfter` roughly five years out. Record that date — Task 6 documents it as the renewal deadline.

- [ ] **Step 6: Export the `.p12`**

In **Keychain Access → login → My Certificates**, right-click **Developer ID Application: …** → **Export "Developer ID Application: …"** → File Format **Personal Information Exchange (.p12)** → save to `~/Desktop/ConvertBar-DeveloperID.p12`.

Set a strong export password and **store it in 1Password immediately** — that password is `APPLE_CERTIFICATE_PASSWORD`. macOS will then also ask for your login-keychain password to release the private key; that one is not needed again.

- [ ] **Step 7: Encode the `.p12` for GitHub**

```bash
base64 -i ~/Desktop/ConvertBar-DeveloperID.p12 | pbcopy
```

The clipboard now holds `APPLE_CERTIFICATE`.

- [ ] **Step 8: Store the three values as repository secrets**

At <https://github.com/rhurling/convertbar/settings/secrets/actions>, create:

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE` | the base64 blob from Step 7 |
| `APPLE_CERTIFICATE_PASSWORD` | the export password from Step 6 |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: Rouven Hurling (ABCDE12345)` — exactly as printed in Step 4 |

`APPLE_SIGNING_IDENTITY` is not cryptographically secret, but it carries a legal name and team ID; keeping it a secret rather than an inline literal keeps both out of public build logs.

- [ ] **Step 9: Prove the CI import path works, before CI depends on it**

The base64 `.p12` + password pair is the one mechanism that no other step exercises — Task 3 signs from the login keychain instead. A truncated paste or a mistyped password would otherwise surface for the first time during a real release. Replay locally exactly what Tauri does in CI (`create-keychain` → `import` → `set-key-partition-list`):

```bash
P12_B64_FILE=~/Desktop/cert.b64            # paste the Step 7 clipboard here first
KC="$HOME/Library/Keychains/signtest.keychain-db"

base64 --decode < "$P12_B64_FILE" > /tmp/roundtrip.p12
security create-keychain -p testpw signtest.keychain
security unlock-keychain -p testpw signtest.keychain
security import /tmp/roundtrip.p12 -k signtest.keychain -P '<APPLE_CERTIFICATE_PASSWORD>' -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k testpw signtest.keychain
security find-identity -v -p codesigning signtest.keychain
```

Expected: the same `Developer ID Application: …` identity as Step 4, found in the throwaway keychain.

`security import` failing with `MAC verification failed` means the password is wrong; an `unable to read` error means the base64 is truncated. Either way, fix it now rather than during a release.

Then remove every trace:

```bash
security delete-keychain signtest.keychain
rm -f /tmp/roundtrip.p12 "$P12_B64_FILE"
```

- [ ] **Step 10: Clean up the disk**

Put `ConvertBar-DeveloperID.p12` in 1Password, then:

```bash
rm ~/Desktop/ConvertBar-DeveloperID.p12 ~/Desktop/ConvertBar.certSigningRequest ~/Downloads/developerID_application.cer
```

The identity stays in the login keychain, which is what Task 3 uses.

---

### Task 2: Create the App Store Connect API key

**Human task.** This replaces the Apple ID + app-specific password that notarization would otherwise need. The key is team-scoped, individually revocable, and carries no Apple ID login capability.

- [ ] **Step 1: Open the Integrations page — this is a hard gate**

Go to <https://appstoreconnect.apple.com/access/integrations/api> and select the **Team Keys** tab.

**STOP if this page is unavailable to your account.** Do not fall back to `APPLE_ID` + app-specific password — that is explicitly out of scope. The fallback is instead **sign-only, no notarization** (see "Fallback" at the end of this plan): TCC grants still become stable, but downloaded builds keep showing Gatekeeper's "unidentified developer" warning. Raise it as a decision rather than substituting credentials.

- [ ] **Step 2: Generate the key**

**+** → Name: `ConvertBar Notarization` → Access: **Developer** → Generate.

Developer is the least privilege that notarization accepts. Do not grant Admin.

- [ ] **Step 3: Capture the two identifiers**

- **Issuer ID** — the UUID printed above the keys table → `APPLE_API_ISSUER`
- **Key ID** — the value in the *Key ID* column of the new row → `APPLE_API_KEY`

- [ ] **Step 4: Download the private key — one chance only**

Reload the page; a **Download** button appears on the new row. It works exactly once. Save `AuthKey_<KEYID>.p8` and put it in 1Password before doing anything else.

- [ ] **Step 5: Prove the key authenticates**

```bash
xcrun notarytool history \
  --key ~/Downloads/AuthKey_<KEYID>.p8 \
  --key-id <KEYID> \
  --issuer <ISSUER-UUID>
```

Expected: a successful response with an empty history (`No submissions found` or an empty list).

Expected failures and their meanings:
- `HTTP status code: 401` → issuer ID and key ID are swapped, or the `.p8` does not match the key ID
- `The provided entity includes an attribute with an invalid value` → the issuer UUID has a typo
- `Unable to find file` → wrong path to the `.p8`

- [ ] **Step 6: Encode the key and store all three secrets**

```bash
base64 -i ~/Downloads/AuthKey_<KEYID>.p8 | pbcopy
```

Add at <https://github.com/rhurling/convertbar/settings/secrets/actions>:

| Secret | Value |
|---|---|
| `APPLE_API_ISSUER` | the issuer UUID |
| `APPLE_API_KEY` | the key ID |
| `APPLE_API_KEY_P8` | the base64 blob from this step |

`APPLE_API_KEY_P8` is a name of our own choosing — Tauri reads a *path* (`APPLE_API_KEY_PATH`), so the workflow decodes this secret to a file and points that variable at it (Task 4).

- [ ] **Step 7: Keep the `.p8` locally for Task 3, then remove it**

Task 3 needs the file. Move it somewhere stable for now:

```bash
mkdir -p ~/private_keys && mv ~/Downloads/AuthKey_<KEYID>.p8 ~/private_keys/
```

Task 3's final step deletes it.

---

### Task 3: Prove both credentials end to end, locally

The point of this task is a **fast failing test**. A full `tauri build` takes many minutes; signing and notarizing a three-line C program takes about two. Prove the credentials on the cheap artifact first, then confirm the real one.

**Files:** none in the repo — this task produces knowledge, recorded in Step 6.

- [ ] **Step 1: Set up the shell**

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Rouven Hurling (ABCDE12345)"
export APPLE_API_ISSUER="<ISSUER-UUID>"
export APPLE_API_KEY="<KEYID>"
export APPLE_API_KEY_PATH="$HOME/private_keys/AuthKey_<KEYID>.p8"
```

`APPLE_CERTIFICATE` is deliberately **not** set locally — the identity is already in the login keychain, and setting it would send Tauri down the CI keychain-import path for no reason.

- [ ] **Step 2: Sign a throwaway binary**

```bash
printf 'int main(void){return 0;}\n' > /tmp/signtest.c
cc -o /tmp/signtest /tmp/signtest.c
codesign --force --options runtime --timestamp -s "$APPLE_SIGNING_IDENTITY" /tmp/signtest
codesign -dvv /tmp/signtest
```

Expected in the `codesign -dvv` output:
- `Authority=Developer ID Application: Rouven Hurling (ABCDE12345)`
- `Authority=Developer ID Certification Authority`
- `flags=0x10000(runtime)` — this is the hardened runtime, which notarization requires
- a `Timestamp=` line — a *secure* timestamp, which notarization also requires

- [ ] **Step 3: Notarize it**

```bash
ditto -c -k --keepParent /tmp/signtest /tmp/signtest.zip
xcrun notarytool submit /tmp/signtest.zip \
  --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" \
  --wait
```

Expected: `status: Accepted`.

`ditto -c -k --keepParent` rather than `zip`: notarization requires the archive to preserve extended attributes and the code signature, which plain `zip` drops.

If it returns `Invalid`, read the actual reason:

```bash
xcrun notarytool log <submission-id> \
  --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER"
```

- `The signature does not include a secure timestamp` → `--timestamp` was missing in Step 2
- `The executable does not have the hardened runtime enabled` → `--options runtime` was missing
- `The binary is not signed with a valid Developer ID certificate` → Task 1 issued the wrong certificate type

Do **not** try to staple this binary. `stapler` only works on bundles, disk images and installer packages; a bare Mach-O has nowhere to put the ticket. That is expected, not a failure.

- [ ] **Step 4: Build the real thing, signed and notarized**

```bash
npm ci
npm run tauri build -- --target aarch64-apple-darwin
```

**This command is expected to exit non-zero. That is not a failure.** `tauri.conf.json` sets `createUpdaterArtifacts: true`, and `TAURI_SIGNING_PRIVATE_KEY` is a CI-only secret, so every local build ends with an updater-signing error *after* bundling and notarization have already completed. `scripts/release.sh:112-115` handles the same thing the same way, and defines success identically.

Success criteria, in the output — not the exit code:
- a `signing` line naming the Developer ID identity
- a notarization submission that waits and is accepted
- a `Finished N bundles` line
- then the expected trailing `TAURI_SIGNING_PRIVATE_KEY` error — ignore it

If notarization is *silently absent* from the output, that is the real failure: the Tauri bundler skips notarization with only a warning when the API-key variables are missing, rather than erroring. Re-check Step 1's exports. (This skip-with-a-warning behaviour is exactly why Task 4's CI preflight is load-bearing.)

This step is slow — a full release build plus a notarization round trip — which is why Steps 2–3 came first.

- [ ] **Step 5: Verify the built bundle**

```bash
APP="target/aarch64-apple-darwin/release/bundle/macos/ConvertBar.app"

codesign -dvv --verbose=4 "$APP"
codesign --verify --strict --verbose=2 "$APP"
spctl -a -vvv --type execute "$APP"
```

Expected from `spctl`: `accepted` and `source=Notarized Developer ID`. Anything else — particularly `source=Unnotarized Developer ID` — means signing worked but notarization did not reach the bundle.

- [ ] **Step 6: Record where the notarization ticket actually landed**

This determines what Task 4's CI verification is allowed to assert. **Run all four and write down the result of each** — do not assume.

```bash
BUNDLE=target/aarch64-apple-darwin/release/bundle

xcrun stapler validate "$APP";                  echo "app -> $?"
xcrun stapler validate "$BUNDLE"/dmg/*.dmg;     echo "dmg -> $?"

# The updater tarball is built from the .app; whether it captured a stapled
# copy depends on whether Tauri tarred before or after stapling.
mkdir -p /tmp/updchk && tar -xzf "$BUNDLE"/macos/ConvertBar.app.tar.gz -C /tmp/updchk
xcrun stapler validate /tmp/updchk/ConvertBar.app;  echo "updater app -> $?"
rm -rf /tmp/updchk
```

**Expected result — confirm it, do not assume it:** the Tauri bundler notarizes and staples the **`.app`** only; the DMG bundler signs the disk image but never submits it for notarization. So `app -> 0` and `dmg -> 1` is the healthy outcome, not a defect. The updater tarball's status is genuinely unknown and depends on whether Tauri tarred before or after stapling.

Whatever the four exit codes actually are, **Task 4 and Task 7 assert exactly that and nothing more.** A CI check asserting an unstapled artifact is stapled would block every future release; an acceptance test asserting the DMG is notarized would fail on a perfectly healthy build.

An unstapled artifact is not a disaster: Gatekeeper falls back to an online notarization check on first launch, so it only affects a user who is offline the very first time they open the app. Record all four results in the PR description.

- [ ] **Step 7: Clean up**

```bash
rm -f /tmp/signtest /tmp/signtest.c /tmp/signtest.zip
rm -f ~/private_keys/AuthKey_<KEYID>.p8
rmdir ~/private_keys 2>/dev/null || true
```

Delete the one file, not the directory — `~/private_keys` is one of the locations Apple tooling searches for App Store Connect keys by convention, so an `rm -rf` there could take out an unrelated key.

The `.p8` is in 1Password and in the `APPLE_API_KEY_P8` secret; nothing needs it on disk.

---

### Task 4: Wire signing into the release workflow

**Files:**
- Modify: `.github/workflows/build.yml`

**Consumes:** the six repository secrets from Tasks 1–2, and the stapling findings from Task 3 Step 6.

- [ ] **Step 1: Add a credential preflight that fails loud**

Insert immediately after the `Install Linux dependencies` step (`.github/workflows/build.yml:58-62`):

```yaml
      # A missing or rotated-out secret would otherwise produce a silently
      # ad-hoc-signed build that ships as a normal release. Refuse instead.
      - name: Require macOS signing credentials
        if: runner.os == 'macOS'
        env:
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}
          APPLE_API_ISSUER: ${{ secrets.APPLE_API_ISSUER }}
          APPLE_API_KEY: ${{ secrets.APPLE_API_KEY }}
          APPLE_API_KEY_P8: ${{ secrets.APPLE_API_KEY_P8 }}
        run: |
          missing=0
          for var in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY \
                     APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_P8; do
            if [ -z "${!var}" ]; then
              echo "::error::$var is not set — refusing to publish an unsigned macOS build"
              missing=1
            fi
          done
          exit $missing
```

This step is the reason the whole change is safe to land: without it, a revoked certificate degrades a release to unsigned without anyone noticing until users' permissions break again.

- [ ] **Step 2: Materialize the API key file**

Directly after the preflight:

```yaml
      # Tauri reads a *path* to the App Store Connect key, so the base64 secret
      # has to become a file. The name must match AuthKey_<KEYID>.p8 because
      # some Apple tooling infers the key ID from the filename.
      - name: Materialize the App Store Connect API key
        if: runner.os == 'macOS'
        env:
          APPLE_API_KEY: ${{ secrets.APPLE_API_KEY }}
          APPLE_API_KEY_P8: ${{ secrets.APPLE_API_KEY_P8 }}
        run: |
          key_path="$RUNNER_TEMP/AuthKey_${APPLE_API_KEY}.p8"
          printf '%s' "$APPLE_API_KEY_P8" | base64 --decode > "$key_path"
          chmod 600 "$key_path"
          echo "APPLE_API_KEY_PATH=$key_path" >> "$GITHUB_ENV"
```

`$RUNNER_TEMP` is outside the checkout, so the key cannot be picked up by a bundler glob or committed by accident.

- [ ] **Step 3: Pass the signing environment to the bundler**

Extend the `env:` block of the `tauri-apps/tauri-action` step (`.github/workflows/build.yml:89-93`) to:

```yaml
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
          # macOS code signing. Ignored on the Linux and Windows legs.
          # APPLE_API_KEY_PATH is exported into GITHUB_ENV by an earlier step.
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}
          APPLE_API_ISSUER: ${{ secrets.APPLE_API_ISSUER }}
          APPLE_API_KEY: ${{ secrets.APPLE_API_KEY }}
```

The Tauri 2 CLI imports `APPLE_CERTIFICATE` into a temporary keychain itself — the manual `security create-keychain` dance in the Tauri docs is not needed and is not added here.

- [ ] **Step 4: Assert the shipped bundle is really signed and notarized**

After the `tauri-action` step:

Notarization itself is already gated: Tauri waits for the notarization result and
fails the build when Apple rejects it, so a rejected submission can never reach
the draft. This step therefore hard-fails only on properties that are decidable
**offline and deterministically**, and reports the network-dependent Gatekeeper
verdict as a warning. Hard-failing on `spctl` would make every release depend on
a live CDN lookup from the runner.

```yaml
      # tauri-action has already uploaded to the draft at this point, but the
      # release is only published by the publish-release job, which needs every
      # matrix leg green. Failing here therefore leaves the release a draft.
      - name: Verify the macOS bundle
        if: runner.os == 'macOS'
        run: |
          set -euo pipefail
          # -print -quit, not `| head -1`: no pipe means no SIGPIPE interaction
          # with pipefail, and it cannot silently pick an arbitrary bundle.
          app=$(find target -maxdepth 6 -name 'ConvertBar.app' -type d -print -quit)
          [ -n "$app" ] || { echo "::error::no ConvertBar.app found"; exit 1; }

          codesign --verify --strict --verbose=2 "$app"

          desc=$(codesign -dvv "$app" 2>&1)
          grep -q 'flags=.*runtime' <<<"$desc" \
            || { echo "::error::hardened runtime missing"; exit 1; }
          grep -q 'Authority=Developer ID Application' <<<"$desc" \
            || { echo "::error::not signed with a Developer ID Application cert"; exit 1; }

          # Advisory: needs network, and notarization is already gated above.
          # Captured rather than piped — `spctl | tee | grep -q` goes non-zero
          # under pipefail when grep exits early and tee takes SIGPIPE, which
          # would print this warning on a perfectly healthy build.
          verdict=$(spctl -a -vvv --type execute "$app" 2>&1) || true
          echo "$verdict"
          grep -q 'source=Notarized Developer ID' <<<"$verdict" \
            || echo "::warning::spctl did not report a notarized Developer ID"
```

**Adjust to Task 3 Step 6's findings before committing:** for each artifact that
Step 6 showed as stapled (exit code 0), add a hard `xcrun stapler validate <path>`
line. Stapling is offline-decidable, so anything observed stapled locally is safe
to assert. Add nothing for artifacts Step 6 showed as unstapled — asserting those
would block every future release.

- [ ] **Step 5: Lint the workflow**

```bash
npx --yes @action-validator/cli --verbose .github/workflows/build.yml
```

Expected: no errors. If `@action-validator` is unavailable offline, fall back to `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/build.yml'))"` — that catches indentation mistakes, which are the realistic failure mode here.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/build.yml
git commit -F - <<'EOF'
ci: sign and notarize macOS release builds

Developer ID signing with App Store Connect API key notarization, so TCC
grants survive rebuilds and Gatekeeper stops blocking downloaded builds.
A preflight fails the job when a credential is missing, because the silent
failure mode is shipping an ad-hoc-signed release that looks fine.
EOF
```

---

### Task 5: Keep secret material out of the repository

**Files:**
- Modify: `.gitignore`

- [ ] **Step 1: Confirm the gap is real**

```bash
grep -nE 'p12|\.p8|\.cer|certSigningRequest' .gitignore || echo "no cert patterns — gap confirmed"
```

Expected: `no cert patterns — gap confirmed`.

- [ ] **Step 2: Add the patterns**

Append to `.gitignore`:

```gitignore

# Code-signing material — must never be committed (see docs/superpowers/plans/2026-07-29-macos-code-signing.md)
*.p12
*.p8
*.cer
*.certSigningRequest
```

- [ ] **Step 3: Verify the ignore actually matches**

```bash
touch AuthKey_TEST.p8 && git check-ignore -v AuthKey_TEST.p8; rm AuthKey_TEST.p8
```

Expected: a line naming `.gitignore` and the `*.p8` rule. A silent exit means the pattern did not match — fix it before moving on.

- [ ] **Step 4: Commit**

```bash
git add .gitignore
git commit -m "chore: never commit code-signing material"
```

---

### Task 6: Document the setup and the renewal path

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add a section after "Version Bump Workflow"**

```markdown
## Code Signing (macOS)

Release builds are Developer ID–signed and notarized in CI. This is not cosmetic:
an ad-hoc signature's designated requirement is the binary's cdhash, so macOS
revoked every TCC permission grant on each new build. A Developer ID signature
anchors the requirement to the team and bundle identifier instead, so grants
survive version bumps.

Six repository secrets drive it, all consumed in `.github/workflows/build.yml`:

| Secret | What it is |
|---|---|
| `APPLE_CERTIFICATE` | base64 of the Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | that `.p12`'s export password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: … (TEAMID)` |
| `APPLE_API_ISSUER` | App Store Connect API issuer UUID |
| `APPLE_API_KEY` | App Store Connect API key ID |
| `APPLE_API_KEY_P8` | base64 of `AuthKey_<KEYID>.p8` |

Notarization authenticates with the App Store Connect API key — deliberately
never `APPLE_ID`/`APPLE_PASSWORD`, so no personal Apple ID or app-specific
password exists in CI. The key is team-scoped and revocable on its own.

`tauri.conf.json` intentionally carries no `signingIdentity`: local builds,
including the rebuild inside `scripts/release.sh`, stay ad-hoc and need no
private key. Only CI signs.

**Renewal:** the certificate expires <DATE FROM TASK 1 STEP 5>. Re-run Task 1 of
`docs/superpowers/plans/2026-07-29-macos-code-signing.md` and replace the first
three secrets. A renewed certificate under the same Team ID does **not** cost
users their permissions again — the designated requirement anchors to the team
and bundle identifier, not to this particular certificate. Builds already shipped
keep working after expiry; notarization tickets do not expire with the cert.

**Rotating the API key:** revoke in App Store Connect, generate a replacement, and
update `APPLE_API_ISSUER`/`APPLE_API_KEY`/`APPLE_API_KEY_P8`. Note the preflight
only checks that secrets are *present* — a revoked-but-still-populated key passes
it and fails later at the notarization submission instead. Still loud, just further
along.
```

Replace `<DATE FROM TASK 1 STEP 5>` with the real `notAfter` date.

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: how macOS release signing is set up and renewed"
```

---

### Task 7: Verify on the first real release

**No repo changes.** This is the acceptance test, and it runs on the next genuine version bump — there is deliberately no test tag, because `build.yml` publishes on any `v*` tag and the updater serves `releases/latest`, so a throwaway tag risks offering users a bogus update.

- [ ] **Step 1: Watch the release build**

Run `./scripts/release.sh <version>` as usual, then watch the workflow. The macOS legs now include the preflight, key materialization, and verification steps, and notarization adds roughly two to ten minutes per architecture.

If a macOS leg fails, `publish-tauri` fails, so `publish-release` never runs and the release stays a **draft**. Recovery: delete the draft release and the tag, fix, re-tag.

- [ ] **Step 2: Verify the published artifact as a user would**

Download the `.dmg` from the release page on a machine that has never built the app. **Assess the `.app` inside it, not the `.dmg` itself** — Tauri notarizes and staples the app bundle, and does not submit the disk image, so a Gatekeeper assessment of the DMG reports un-notarized on a completely healthy release. (Confirm against Task 3 Step 6's recorded `dmg ->` exit code; if that came back `0`, the DMG *is* stapled and can be assessed directly.)

```bash
xattr -p com.apple.quarantine ~/Downloads/ConvertBar_*.dmg   # confirms it is quarantined, like a user's copy
hdiutil attach ~/Downloads/ConvertBar_*.dmg
spctl -a -vvv --type execute /Volumes/ConvertBar/ConvertBar.app
hdiutil detach /Volumes/ConvertBar
```

Expected: `accepted`, `source=Notarized Developer ID`.

Then drag it to `/Applications` and open it. That launch is the assertion that matters: there must be **no** "cannot be opened because the developer cannot be verified" dialog and no workaround needed.

- [ ] **Step 3: Verify the reason this work exists**

Install the signed build, grant it a permission that TCC mediates (a watched folder under `~/Documents` or `~/Desktop`), then ship the *next* release and update into it.

Expected: the grant survives. That is the acceptance criterion for the whole plan — signing that does not stabilize TCC has not solved the problem it was undertaken for.

Note that the transition itself costs the grants once: the currently installed ad-hoc build's permissions are lost when the first signed build replaces it. That is expected and happens exactly once.

---

## Fallback: sign without notarizing

Only if Task 2 Step 1 finds App Store Connect API keys unavailable. Do not use this to avoid a legitimate blocker — raise the blocker first.

Set `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD` and `APPLE_SIGNING_IDENTITY`, then:

- **Trim Task 4 Step 1's preflight loop to those three variables.** Leaving all six listed makes the preflight fail on every release forever, since the three API-key secrets no longer exist.
- Omit the three API-key secrets, Task 4 Step 2 (key materialization), and Task 4 Step 3's `APPLE_API_ISSUER`/`APPLE_API_KEY` lines.
- Drop the `source=Notarized Developer ID` advisory from Task 4 Step 4. Keep `codesign --verify`, the hardened-runtime check and the Developer ID authority check — all three still hold.
- Skip Task 3 Step 3 and adjust Task 7 Step 2 to expect a Gatekeeper warning rather than a clean launch.

What that buys and costs:
- **Buys:** stable TCC grants — the entire original motivation.
- **Costs:** Gatekeeper still blocks downloaded builds with "cannot be opened because the developer cannot be verified". On macOS 15 and later this is worse than the familiar right-click → Open: that override was removed, and each user must go to System Settings → Privacy & Security → "Open Anyway" per app.

Notarization can be added later without redoing Task 1.

## Out of Scope

- **Windows signing.** Needs Azure Trusted Signing (~$10/month, organization or individual identity verification) or an EV certificate on a cloud HSM. Separate plan.
- **Linux.** Nothing to sign; the updater's minisign signature already covers artifact integrity.
- **Local signing.** Deliberately excluded so `scripts/release.sh` keeps working on a machine without the private key. If local TCC-grant churn during development becomes annoying, that is a follow-up decision, not part of this plan.
- **App Store distribution.** Requires a different certificate type, provisioning profiles, and full sandboxing.
