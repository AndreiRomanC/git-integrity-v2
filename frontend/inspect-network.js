// Complete history is ordered newest first. Draw older commits to the left,
// carrying the graph model's open parent lanes through each column.
function inspectNetworkHtml(model, commits, tips, defaultRef) {
  const esc = escapeSubmoduleVersionHtml;
  const y = lane => 68 + lane * 58;
  const xs = [], labels = [];
  let width = 30, lastDateX = -Infinity, previousDate = '', maxLane = 0;
  const dates = [], edges = [], nodes = [];
  const byId = new Map(model.map(row => [row.commitId, row]));
  for (let i = model.length - 1; i >= 0; i--) {
    const row = model[i], commit = commits[i];
    const branches = (tips.get(row.commitId) || []).filter(ref => ref.kind === 'branch').map(ref => ref.name);
    const first = branches.includes(defaultRef?.name) ? defaultRef.name : branches[0];
    const short = first && (first.length > 23 ? first.slice(0, 22) + '…' : first);
    const label = short ? short + (branches.length > 1 ? ' +' + (branches.length - 1) : '') : '';
    xs[i] = width;
    labels[i] = { label, branches };
    const day = String(commit.date || '').slice(0, 10);
    if (day && day !== previousDate && width - lastDateX >= 96) {
      dates.push(`<text class="network-date" x="${width}" y="22">${esc(day)}</text>`);
      lastDateX = width; previousDate = day;
    }
    width += Math.max(32, label.length * 7.6 + 22);
    maxLane = Math.max(maxLane, row.lane, row.after.length - 1);
  }
  const color = lane => ['#77baff', '#66d3af', '#c3a0ee', '#efb66f', '#77cfdc'][lane % 5];
  model.forEach((row, i) => {
    const commit = commits[i], x = xs[i];
    for (const parent of row.parents) {
      const destination = byId.get(parent.commitId);
      const end = destination ? xs[destination.row] : 8;
      const turn = xs[i + 1] ?? end;
      const mid = (x + turn) / 2;
      const startY = y(row.lane), endY = y(parent.targetLane);
      const path = `M${x} ${startY} C${mid} ${startY} ${mid} ${endY} ${turn} ${endY} H${end}`;
      edges.push(`<path d="${path}" stroke="${color(Math.max(row.lane, parent.targetLane))}"${destination ? '' : ' stroke-dasharray="3 4"'}><title>${destination ? 'Parent ' : 'Parent outside loaded history: '}${esc(parent.commitId)}</title></path>`);
    }
    const { label, branches } = labels[i];
    const subtitle = [commit.author, commit.date, row.commitId.slice(0, 12)].filter(Boolean).join(' · ');
    const title = [commit.subject, subtitle, branches.join(', ')].filter(Boolean).join('\n');
    const tip = label ? `<rect x="-7" y="-32" width="${label.length * 7.6 + 13}" height="20" rx="5"/><text x="0" y="-18">${esc(label)}</text>` : '';
    nodes.push(`<g transform="translate(${x} ${y(row.lane)})" tabindex="0" role="button" data-revision="${esc(row.commitId)}" data-name="${esc(commit.name)}" data-version-kind="commit" aria-label="${esc(title)}">
      <title>${esc(title)}</title>${tip}<circle r="12" fill="transparent"/><circle class="network-dot" r="${row.parents.length > 1 ? 6 : 4.5}" fill="${color(row.lane)}"/></g>`);
  });
  width = Math.max(width + 65, 240);
  return `<svg class="inspect-network" width="${width}" height="${y(maxLane) + 36}" role="group" aria-label="Commit network, oldest to newest"><g>${dates.join('')}</g><g fill="none" stroke-width="2">${edges.join('')}</g>${nodes.join('')}</svg>`;
}

function bindInspectNetworkPan(scroller, active) {
  let drag, suppressClick = false;
  scroller.addEventListener('pointerdown', event => {
    suppressClick = false;
    if (!active() || event.button !== 0 || !event.isPrimary || event.pointerType === 'touch') return;
    drag = { id: event.pointerId, x: event.clientX, left: scroller.scrollLeft, moved: false };
  });
  scroller.addEventListener('pointermove', event => {
    if (event.pointerId !== drag?.id) return;
    if (!event.buttons) { drag = null; return; }
    const distance = event.clientX - drag.x;
    if (!drag.moved && Math.abs(distance) < 5) return;
    if (!drag.moved) { scroller.setPointerCapture(event.pointerId); drag.moved = true; }
    scroller.scrollLeft = drag.left - distance;
  });
  const finish = event => {
    if (event.pointerId !== drag?.id) return;
    suppressClick = drag.moved;
    drag = null;
  };
  scroller.addEventListener('pointerup', finish);
  scroller.addEventListener('pointercancel', finish);
  scroller.addEventListener('click', event => {
    if (suppressClick && event.detail) { event.stopPropagation(); event.preventDefault(); }
    suppressClick = false;
  }, true);
}
