// Delegated row click for panel v2 agent rows.
// CSP-safe (script-src 'self'); paired with data-href in panel.rs.
document.addEventListener('click', (e) => {
  const row = e.target.closest('tr.agent-row[data-href]');
  if (row && !e.target.closest('a')) {
    location.href = row.dataset.href;
  }
});
