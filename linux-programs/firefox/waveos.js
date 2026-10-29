// Firefox ESR on WaveOS: default preferences, read from /etc/firefox-esr
// (xtask puts this file there in the Linux packages). They are defaults:
// what you change in Settings or about:config wins.

// No welcome tour, no "what's new" page, no default-browser question.
pref("browser.aboutwelcome.enabled", false);
pref("startup.homepage_welcome_url", "");
pref("startup.homepage_welcome_url.additional", "");
pref("browser.startup.homepage_override.mstone", "ignore");
pref("browser.shell.checkDefaultBrowser", false);
// Start with a new tab even when Firefox didn't close normally (a shut-down
// virtual machine), not a "restore session" page.
pref("browser.sessionstore.resume_from_crash", false);

// WaveOS has no user namespaces, so Firefox's sandboxes are off (see
// firefox.sh); don't show the warning bar about it on every start.
pref("security.sandbox.warn_unprivileged_namespaces", false);

// A compact, quiet toolbar: no account or Pocket buttons and no bookmarks
// bar.
pref("browser.compactmode.show", true);
pref("browser.uidensity", 1);
pref("identity.fxaccounts.enabled", false);
pref("extensions.pocket.enabled", false);
pref("browser.toolbars.bookmarks.visibility", "never");

// A clean new tab: just a search box (no site tiles, nothing sponsored).
pref("browser.newtabpage.activity-stream.showSponsored", false);
pref("browser.newtabpage.activity-stream.showSponsoredTopSites", false);
pref("browser.newtabpage.activity-stream.feeds.section.topstories", false);
pref("browser.newtabpage.activity-stream.feeds.section.highlights", false);
pref("browser.newtabpage.activity-stream.feeds.snippets", false);
pref("browser.newtabpage.activity-stream.asrouter.userprefs.cfr.addons", false);
pref("browser.newtabpage.activity-stream.asrouter.userprefs.cfr.features", false);

// Nothing sent to Mozilla.
pref("datareporting.policy.dataSubmissionEnabled", false);
pref("datareporting.healthreport.uploadEnabled", false);
pref("toolkit.telemetry.enabled", false);
pref("toolkit.telemetry.reportingpolicy.firstRun", false);
pref("app.shield.optoutstudies.enabled", false);

// Downloads go to WaveOS's Downloads folder, where Files shows them.
pref("browser.download.folderList", 2);
pref("browser.download.dir", "/Downloads");

// Fewer processes. Each one starts Firefox from scratch, which takes long
// on WaveOS (in QEMU, tens of seconds), and the sandboxes that would set
// them apart are off anyway: pages share a few content processes instead
// of one per site, and the new-tab page, networking and extensions run in
// processes that are there already.
pref("fission.autostart", false);
pref("dom.ipc.processCount", 4);
pref("browser.tabs.remote.separatePrivilegedContentProcess", false);
pref("network.process.enabled", false);
pref("extensions.webextensions.remote", false);
