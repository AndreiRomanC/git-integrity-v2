// Display-only helpers. Timestamps come from Git/process metadata, never from
// a guessed midnight or the time at which a status request happens to finish.
(function (root) {
  const pad = value => String(value).padStart(2, '0');
  const localDateTime = new Intl.DateTimeFormat(undefined, {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', timeZoneName: 'short',
  });
  function validDate(milliseconds) {
    if (typeof milliseconds !== 'number' || !Number.isFinite(milliseconds)) return null;
    const date = new Date(milliseconds);
    return Number.isNaN(date.getTime()) ? null : date;
  }
  function clock(milliseconds) {
    const date = validDate(milliseconds);
    return date ? `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}` : 'time unavailable';
  }
  function commitTime(commit = {}) {
    const date = validDate(typeof commit.timestamp === 'number' ? commit.timestamp * 1000 : null);
    if (!date) return { date: commit.date || '—', time: '', title: 'Commit time unavailable in this cached entry', iso: '' };
    return {
      date: `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`,
      time: `${pad(date.getHours())}:${pad(date.getMinutes())}`,
      title: `Commit time (your local timezone): ${localDateTime.format(date)}`,
      iso: date.toISOString(),
    };
  }
  const api = { clock, commitTime };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.GitActivity = api;
})(typeof globalThis !== 'undefined' ? globalThis : this);
