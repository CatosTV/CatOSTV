// CatosTV YouTube cast profile (opened by catostv-castd for YouTube "link with TV"). Applied by Firefox on every start.
//
// youtube.com/tv serves its 10-foot (remote-friendly) interface only to TV
// user agents; desktop browsers are redirected to the normal site. This
// profile therefore identifies as a smart-TV browser.
// STATUS: UNVERIFIED on real hardware.
user_pref("general.useragent.override", "Mozilla/5.0 (SMART-TV; Linux; Tizen 6.0) AppleWebKit/538.1 (KHTML, like Gecko) Version/6.0 TV Safari/538.1");
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.aboutwelcome.enabled", false);
user_pref("datareporting.policy.dataSubmissionPolicyBypassNotification", true);
user_pref("browser.sessionstore.resume_from_crash", false);
user_pref("media.autoplay.default", 0);
user_pref("media.ffmpeg.vaapi.enabled", true);
