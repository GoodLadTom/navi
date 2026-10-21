// Which system Navi is running on, read once from the webview (WebKit on a Mac says so in its
// user agent), so each window can use that system's words and keys. The root gets a "mac" class
// for the styles that differ (the Mac keeps its own traffic-light buttons).
export const isMac = /Macintosh|Mac OS X/.test(navigator.userAgent);
if (isMac) document.documentElement.classList.add('mac');

// `windows` on Windows, `mac` on a Mac.
export const os = (windows, mac) => (isMac ? mac : windows);
