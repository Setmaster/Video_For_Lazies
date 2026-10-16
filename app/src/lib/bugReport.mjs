const BUG_REPORT_URL = "https://vfl-bugs.7dev.io/";

export function buildBugReportUrl(version, platform = "") {
  // Keep browser/OS details local; only send a coarse platform label.
  const coarsePlatform = /^Win/i.test(platform)
    ? "Windows"
    : /^Mac/i.test(platform)
      ? "macOS"
      : /^Linux/i.test(platform)
        ? "Linux"
        : "Other";
  const url = new URL(BUG_REPORT_URL);
  url.searchParams.set("version", version);
  url.searchParams.set("platform", coarsePlatform);
  return url.toString();
}
