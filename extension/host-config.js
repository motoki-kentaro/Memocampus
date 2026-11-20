// The one place that names the Native Messaging host this extension talks to (Issue #67, Slice 3B-2).
//
// Three host names exist, and they are three separate registrations (each is its own key under
// HKCU\Software\Google\Chrome\NativeMessagingHosts):
//   product   io.github.memocampus.vault       registered by the Windows installer for the Web Store build
//   dev       io.github.memocampus.vault.dev    registered by native-host/install-native-host.ps1 -HostName ...
//   legacyDev com.memocampus.vault              the name development setups used before this slice
//
// HOST_PROFILE picks one. It is a single constant, edited by scripts/set-host-profile.mjs and by nothing
// at run time: there is no fallback from one name to another, and a wrong or missing value is an error at
// load time, never a silent switch to another host. The value committed in the repository is `legacyDev`
// so an unpacked development extension keeps connecting to the registration it already has. The Web Store
// package is built with `node scripts/set-host-profile.mjs product` (see installer/README.md).
export const HOST_NAMES = Object.freeze({
  product: "io.github.memocampus.vault",
  dev: "io.github.memocampus.vault.dev",
  legacyDev: "com.memocampus.vault",
});

export const HOST_PROFILE = "legacyDev";

export const HOST_NAME = hostNameFor(HOST_PROFILE);

export function hostNameFor(profile) {
  if (!Object.prototype.hasOwnProperty.call(HOST_NAMES, profile)) {
    throw new Error(`Unknown native host profile: ${String(profile)}`);
  }
  return HOST_NAMES[profile];
}
