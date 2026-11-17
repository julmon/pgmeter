//! The "System" section of the HTML report: the series of each host given
//! to `--sysmon`, on the report's time axis, and the page fragments that
//! draw them.

use serde_json::{json, Value};

use super::sysmon::delta::{DiskSample, Sample};
use super::sysmon::window::{self, HostSummary};
use super::sysmon::{Device, HostSeries, TimedSample};

const KB_PER_GIB: f64 = 1_048_576.0;

/// One value per bucket of the time axis: the mean of the samples that fall
/// in it, None where there is none (before the sampler's first interval,
/// after it died).
struct Buckets<'a> {
    samples: &'a [TimedSample],
    origin: i64,
    bucket_secs: u64,
    len: usize,
}

impl Buckets<'_> {
    fn series(&self, pick: impl Fn(&Sample) -> Option<f64>) -> Vec<Option<f64>> {
        let mut sums = vec![(0.0_f64, 0_u32); self.len];
        for timed in self.samples {
            let elapsed = timed.timestamp - self.origin as f64;
            if elapsed < 0.0 {
                continue;
            }
            let index = (elapsed / self.bucket_secs as f64) as usize;
            if let (Some(slot), Some(value)) = (sums.get_mut(index), pick(&timed.sample)) {
                slot.0 += value;
                slot.1 += 1;
            }
        }
        sums.into_iter()
            .map(|(sum, count)| {
                // Three decimals are plenty for a chart and keep the page small.
                (count > 0).then(|| (sum / f64::from(count) * 1000.0).round() / 1000.0)
            })
            .collect()
    }

    fn disk(&self, device: &str, pick: impl Fn(&DiskSample) -> Option<f64>) -> Vec<Option<f64>> {
        self.series(|sample| {
            sample
                .disks
                .iter()
                .find(|d| d.device == device)
                .and_then(&pick)
        })
    }
}

fn display_name(device: &str, devices: &[Device]) -> String {
    devices
        .iter()
        .find(|d| d.name == device)
        .and_then(|d| d.mapper_name.clone())
        .unwrap_or_else(|| device.to_string())
}

/// Every device under `device`, layer by layer, without repeats.
fn layers_under(device: &str, devices: &[Device], found: &mut Vec<String>) {
    let Some(entry) = devices.iter().find(|d| d.name == device) else {
        return;
    };
    for below in &entry.on {
        if !found.contains(below) {
            found.push(below.clone());
            layers_under(below, devices, found);
        }
    }
}

/// The section's data for one host. `origin` is the Unix second of the time
/// axis' zero and `axis` the start of each bucket, in seconds from it.
fn host_json(series: &HostSeries, summary: &HostSummary, origin: i64, axis: &[u64]) -> Value {
    let bucket_secs = match axis {
        [first, second, ..] => (second - first).max(1),
        _ => 1,
    };
    let buckets = Buckets {
        samples: &series.samples,
        origin,
        bucket_secs,
        len: axis.len(),
    };
    let info = &series.info;

    let cpu = |pick: fn(&super::sysmon::delta::CpuShares) -> f64| {
        buckets.series(move |s| s.cpu.as_ref().map(pick))
    };
    let gauge_gib = |field: &'static str| {
        buckets.series(move |s| Some(s.memory.gauge(field)? as f64 / KB_PER_GIB))
    };
    let gauge_mib =
        |field: &'static str| buckets.series(move |s| Some(s.memory.gauge(field)? as f64 / 1024.0));
    let rate = |field: &'static str| buckets.series(move |s| s.memory.rate(field));
    let oom_kills: f64 = series
        .samples
        .iter()
        .filter_map(|s| Some(s.sample.memory.rate("oom_kill")? * s.sample.interval_s))
        .sum();

    // The volumes of the summary first, then the layers under them, which
    // carry the same I/O and are only reached from the device selector.
    let mut lower: Vec<String> = Vec::new();
    for volume in &summary.volumes {
        layers_under(&volume.device, &info.devices, &mut lower);
    }
    lower.retain(|device| !summary.volumes.iter().any(|v| v.device == *device));

    let disk_json = |device: &str, name: &str, title: String, top: bool, flush: Option<&str>| {
        let flushes = flush.map(|leaf| buckets.disk(leaf, |d| d.f_s));
        let flush_time = flush.map(|leaf| buckets.disk(leaf, |d| d.f_await_ms));
        json!({
            "device": device,
            "name": name,
            "title": title,
            "top": top,
            // Where the flushes drawn for this device are counted, when it
            // is not the device itself.
            "flushName": flush.filter(|leaf| *leaf != device).map(|leaf| display_name(leaf, &info.devices)),
            "rmb": buckets.disk(device, |d| Some(d.rkb_s / 1024.0)),
            "wmb": buckets.disk(device, |d| Some(d.wkb_s / 1024.0)),
            "r": buckets.disk(device, |d| Some(d.r_s)),
            "w": buckets.disk(device, |d| Some(d.w_s)),
            "f": flushes,
            "rAwait": buckets.disk(device, |d| Some(d.r_await_ms)),
            "wAwait": buckets.disk(device, |d| Some(d.w_await_ms)),
            "fAwait": flush_time,
            "rSize": buckets.disk(device, |d| Some(d.rareq_kb)),
            "wSize": buckets.disk(device, |d| Some(d.wareq_kb)),
            "queue": buckets.disk(device, |d| Some(d.aqu_sz)),
            "util": buckets.disk(device, |d| Some(d.util_pct)),
        })
    };
    let mut disks: Vec<Value> = summary
        .volumes
        .iter()
        .map(|v| {
            disk_json(
                &v.device,
                &v.name,
                v.titled(),
                true,
                v.flush_device.as_deref(),
            )
        })
        .collect();
    for device in &lower {
        let flushed = series.samples.iter().any(|s| {
            s.sample
                .disks
                .iter()
                .any(|d| d.device == *device && d.f_s.is_some_and(|f| f > 0.0))
        });
        let name = display_name(device, &info.devices);
        disks.push(disk_json(
            device,
            &name,
            name.clone(),
            false,
            flushed.then_some(device.as_str()),
        ));
    }

    let page_cache = buckets.series(|s| {
        let cached = s.memory.gauge("Cached")? + s.memory.gauge("Buffers")?;
        Some(cached.saturating_sub(s.memory.gauge("Shmem")?) as f64 / KB_PER_GIB)
    });
    // What the classes drawn do not cover: kernel stacks, per-CPU areas,
    // memory the kernel does not itemize.
    let other = buckets.series(|s| {
        let m = &s.memory;
        let cached = (m.gauge("Cached")? + m.gauge("Buffers")?).saturating_sub(m.gauge("Shmem")?);
        let counted = m.gauge("MemFree")?
            + m.gauge("AnonPages")?
            + m.gauge("Shmem")?
            + cached
            + m.gauge("Slab")?
            + m.gauge("PageTables")?;
        Some(m.gauge("MemTotal")?.saturating_sub(counted) as f64 / KB_PER_GIB)
    });

    json!({
        "host": {
            "target": info.target,
            "host": info.host,
            "kernel": info.kernel,
            "cpus": info.cpus,
            "memTotalKb": info.mem_total_kb,
            "intervalS": info.interval_s,
            "samples": series.samples.len(),
            "database": info.database,
            "truncated": series.truncated,
            "truncatedAtS": series.truncated.as_ref().and_then(|_| {
                series.samples.last().map(|s| (s.timestamp - origin as f64).max(0.0).round())
            }),
        },
        "summary": summary.json(),
        "cpu": {
            "user": cpu(|c| c.user),
            "system": cpu(|c| c.system),
            "iowait": cpu(|c| c.iowait),
            "softirq": cpu(|c| c.softirq),
            "irq": cpu(|c| c.irq),
            "nice": cpu(|c| c.nice),
            "steal": cpu(|c| c.steal),
            "guest": cpu(|c| c.guest + c.guest_nice),
            "idle": cpu(|c| c.idle),
            "ctxt": buckets.series(|s| s.activity.ctxt_s),
            "forks": buckets.series(|s| s.activity.forks_s),
            "running": buckets.series(|s| s.activity.procs_running.map(|n| n as f64)),
            "blocked": buckets.series(|s| s.activity.procs_blocked.map(|n| n as f64)),
            "load1": buckets.series(|s| s.activity.load1),
            "psiCpu": buckets.series(|s| s.pressure_cpu.some),
            "psiIo": buckets.series(|s| s.pressure_io.some),
            "psiIoFull": buckets.series(|s| s.pressure_io.full),
            "psiMem": buckets.series(|s| s.pressure_memory.some),
            "psiMemFull": buckets.series(|s| s.pressure_memory.full),
        },
        "mem": {
            "anon": gauge_gib("AnonPages"),
            "shared": gauge_gib("Shmem"),
            "cache": page_cache,
            "slab": gauge_gib("Slab"),
            "pageTables": gauge_gib("PageTables"),
            "other": other,
            "available": gauge_gib("MemAvailable"),
            "dirty": gauge_mib("Dirty"),
            "writeback": gauge_mib("Writeback"),
            // The kernel counts these two in kB.
            "pgin": buckets.series(|s| Some(s.memory.rate("pgpgin")? / 1024.0)),
            "pgout": buckets.series(|s| Some(s.memory.rate("pgpgout")? / 1024.0)),
            "faults": rate("pgfault"),
            "swapIn": rate("pswpin"),
            "swapOut": rate("pswpout"),
            "majFaults": rate("pgmajfault"),
            "oomKills": oom_kills.round(),
        },
        "disks": disks,
    })
}

/// The `system` array of the report's data: one entry per monitored host,
/// in the order of the `--sysmon` options. Empty without a time axis.
pub fn system_json(
    system: &[HostSeries],
    window_start: Option<i64>,
    origin: Option<i64>,
    axis: &[u64],
) -> Value {
    let Some(origin) = origin else {
        return json!([]);
    };
    Value::Array(
        system
            .iter()
            .map(|series| {
                let summary = window::summarize(series, window_start);
                host_json(series, &summary, origin, axis)
            })
            .collect(),
    )
}

/// The section's styles, added to the report's.
pub const SYSTEM_CSS: &str = r##"
  h3 { font-size: 13px; margin: 28px 0 10px; color: var(--muted); text-transform: uppercase;
    letter-spacing: .06em; font-weight: 600; }
  .sys-head { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px 16px;
    border-bottom: 1px solid var(--border); margin: 36px 0 14px; padding-bottom: 8px; }
  .sys-head h2 { margin: 0; border: 0; padding: 0; }
  .sys-head label, .sys-group label { color: var(--muted); font-size: 12px; }
  .sys-head select, .sys-group select { background: var(--panel2); color: var(--text); border: 1px solid var(--border);
    border-radius: 6px; padding: 5px 8px; font-size: 12px; margin-left: 6px; max-width: 100%; }
  .sys-group { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: 8px 16px; }
  .sys-status { margin: 0 0 14px; padding: 10px 12px; border: 1px solid var(--warn); border-radius: 8px;
    font-size: 13px; line-height: 1.45; }
  /* Six cards in each row, on the same columns: 6, 3 or 2 across, so a row
     is always full and the two rows line up. */
  .sys-six { grid-template-columns: repeat(6, minmax(0, 1fr)); }
  @media (max-width: 960px) { .sys-six { grid-template-columns: repeat(3, minmax(0, 1fr)); } }
  @media (max-width: 520px) { .sys-six { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  .sys-card { min-width: 0; }
  .sys-card .k { color: var(--muted); font-size: 12px; }
  .sys-card .v { font-size: 14px; margin-top: 2px; word-break: break-word; }
  .sys-card .label { color: var(--muted); font-size: 12px; text-transform: uppercase; letter-spacing: .04em;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .sys-card .value { font-size: 22px; font-weight: 700; margin-top: 6px; white-space: nowrap;
    font-variant-numeric: tabular-nums; }
  .sys-card .sub { color: var(--muted); font-size: 12px; margin-top: 2px; overflow-wrap: anywhere;
    font-variant-numeric: tabular-nums; }
  .sys-card .which { font-size: 12px; margin-top: 8px; padding-top: 8px; border-top: 1px solid var(--border);
    overflow-wrap: anywhere; line-height: 1.35; }
  .sys-grid { grid-template-columns: repeat(auto-fit, minmax(min(440px, 100%), 1fr)); }
  .sys-grid .card { min-width: 0; }
  .sys-grid .wide { grid-column: 1 / -1; }
  .sys-volumes { overflow-x: auto; }
  .sys-volumes table { font-variant-numeric: tabular-nums; }
  .sys-volumes th, .sys-volumes td { white-space: nowrap; }
  .sys-volumes th:nth-child(-n+3), .sys-volumes td:nth-child(-n+3) { text-align: left; }
  .sys-volumes td:first-child { white-space: normal; overflow-wrap: anywhere; min-width: 120px; }
  .sys-volumes td:nth-child(3) { color: var(--muted); white-space: normal; min-width: 110px; }
  .sys-legend .item .stroke { width: 16px; height: 0; border-top: 2px solid; flex: 0 0 auto; }
  .sys-legend .item .stroke.dash { border-top-style: dashed; }
"##;

/// The section's markup, placed after the report's own charts. Hidden until
/// the script finds a monitored host.
pub const SYSTEM_HTML: &str = r##"
  <div id="system-view" hidden>
  <div class="sys-head"><h2 id="sys-title">System</h2>
    <label id="sys-host-label">Host<select id="sys-host"></select></label></div>
  <p class="sys-status" id="sys-status" hidden></p>
  <div class="grid sys-six" id="sys-meta"></div>
  <div class="grid sys-six" id="sys-kpis" style="margin-top:16px"></div>
  <p class="note">The figures above cover the measured window only, after the ramp-up, like every other figure of the report.</p>

  <h3>CPU</h3>
  <div class="grid sys-grid">
    <div class="card chart-card line-card wide"><div class="cwrap"><canvas id="sys-cpu"></canvas></div><div class="legend sys-legend" id="sys-cpu-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-load"></canvas></div><div class="legend sys-legend" id="sys-load-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-psi"></canvas></div><div class="legend sys-legend" id="sys-psi-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-ctxt"></canvas></div><div class="legend sys-legend" id="sys-ctxt-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-forks"></canvas></div><div class="legend sys-legend" id="sys-forks-legend"></div></div>
  </div>
  <p class="note" id="sys-cpu-note"></p>

  <h3>Memory</h3>
  <div class="grid sys-grid">
    <div class="card chart-card line-card wide"><div class="cwrap"><canvas id="sys-mem"></canvas></div><div class="legend sys-legend" id="sys-mem-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-dirty"></canvas></div><div class="legend sys-legend" id="sys-dirty-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-paging"></canvas></div><div class="legend sys-legend" id="sys-paging-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-faults"></canvas></div><div class="legend sys-legend" id="sys-faults-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-swap"></canvas></div><div class="legend sys-legend" id="sys-swap-legend"></div></div>
  </div>
  <p class="note" id="sys-mem-note"></p>

  <div id="sys-disk">
  <h3>Disk</h3>
  <div class="card sys-volumes">
    <table>
      <thead><tr>
        <th>Volume</th><th>Holds</th><th>On</th><th>Written MB/s</th><th>Read MB/s</th><th>Requests/s</th>
        <th>Write time ms</th><th>Flush time ms</th><th>Busy %</th><th>Max %</th>
      </tr></thead>
      <tbody id="sys-volumes-body"></tbody>
    </table>
  </div>
  <p class="note">Averages over the measured window, one row per volume. Flush time is measured at the disk under the volume. "Holds" is where PostgreSQL keeps its data files and its WAL, when pgmeter could read the server's data directory.</p>
  <div class="sys-group"><h3>Charts by device</h3>
    <label>Device<select id="sys-device"></select></label></div>
  <div class="grid sys-grid">
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-dthr"></canvas></div><div class="legend sys-legend" id="sys-dthr-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-diops"></canvas></div><div class="legend sys-legend" id="sys-diops-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-dawait"></canvas></div><div class="legend sys-legend" id="sys-dawait-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-dsize"></canvas></div><div class="legend sys-legend" id="sys-dsize-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-dqueue"></canvas></div><div class="legend sys-legend" id="sys-dqueue-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="sys-dutil"></canvas></div><div class="legend sys-legend" id="sys-dutil-legend"></div></div>
  </div>
  <p class="note" id="sys-disk-note"></p>
  </div>
  </div>
"##;

/// The section's script. It runs after the report's charts are created and
/// before their download buttons are, and registers its own charts in the
/// same `charts` registry. `renderSystem` is called again when the host, the
/// device or the theme changes.
pub const SYSTEM_JS: &str = r##"
// --- System section ---------------------------------------------------------
// A colour per CPU state, memory class and I/O direction, the same on every
// host. The set is its own: it keeps stacked areas apart for colour-blind
// readers, on the dark panel and on the light one.
const SYS_COLORS = {
  dark:  ['#3987e5', '#d95926', '#199e70', '#c98500', '#d55181', '#0ca30c', '#9085e9', '#e66767'],
  light: ['#2a78d6', '#eb6834', '#1baf7a', '#eda100', '#e87ba4', '#008300', '#6250d6', '#e34948'],
};
const SYS = DATA.system || [];
const sysHostSelect = document.getElementById('sys-host');
const sysDeviceSelect = document.getElementById('sys-device');
let sysShownHost = -1;

function sysCards(boxId, rows) {
  const box = document.getElementById(boxId);
  box.replaceChildren();
  for (const row of rows) {
    const el = document.createElement('div');
    el.className = 'card sys-card';
    for (const [cls, text] of row) {
      if (text == null || text === '') continue;
      const d = document.createElement('div');
      d.className = cls;
      d.textContent = text;
      el.appendChild(d);
    }
    box.appendChild(el);
  }
}

// The legend under a chart: a square for an area, a stroke for a line,
// dashed like the line. One series needs none when the title names it.
function sysLegend(id, chart, always) {
  const box = document.getElementById(id + '-legend');
  box.replaceChildren();
  box.hidden = chart.data.datasets.length < 2 && !always;
  if (box.hidden) return;
  chart.data.datasets.forEach((ds, i) => {
    const el = document.createElement('div');
    el.className = 'item';
    const sw = document.createElement('span');
    if (ds.fill) { sw.className = 'swatch'; sw.style.background = ds.backgroundColor; }
    else { sw.className = 'stroke' + (ds.borderDash ? ' dash' : ''); sw.style.borderTopColor = ds.borderColor; }
    const txt = document.createElement('span');
    txt.textContent = ds.label;
    el.append(sw, txt);
    el.onclick = () => {
      const visible = !chart.isDatasetVisible(i);
      chart.setDatasetVisibility(i, visible);
      el.classList.toggle('hidden', !visible);
      chart.update();
    };
    box.appendChild(el);
  });
}

function sysChart(id, title, yTitle, datasets, o = {}) {
  if (charts[id]) charts[id].destroy();
  const panel = (getComputedStyle(document.documentElement).getPropertyValue('--panel') || '#1e293b').trim();
  const unit = o.unit || '';
  const decimals = o.decimals == null ? 1 : o.decimals;
  const sets = datasets.map((ds, i) => o.stacked
    ? {label: ds.label, data: ds.data, backgroundColor: ds.color, borderColor: panel, borderWidth: 1.5,
       fill: i === 0 ? 'origin' : '-1', cubicInterpolationMode: 'monotone'}
    : {label: ds.label, data: ds.data, borderColor: ds.color, backgroundColor: ds.color,
       borderWidth: ds.width || 1.5, borderDash: ds.dash, fill: false});
  const opts = lineOpts(yTitle);
  opts.animation = false;
  opts.scales.x.ticks = {maxTicksLimit: 15};
  opts.scales.y.stacked = !!o.stacked;
  if (o.yMax != null) { opts.scales.y.max = o.yMax; opts.scales.y.ticks = {includeBounds: false}; }
  opts.plugins = {
    legend: {display: false},
    title: {display: true, text: title},
    tooltip: {callbacks: {
      title: (items) => `${items[0].label} s`,
      label: (item) => `${item.dataset.label}: ${item.parsed.y == null ? '-' : fmt(item.parsed.y, decimals)}${unit}`,
      footer: o.footer || (() => ''),
    }},
  };
  charts[id] = new Chart(document.getElementById(id), {
    type: 'line',
    data: {labels: DATA.timeAxis, datasets: sets},
    options: opts,
    plugins: [rampupPlugin],
  });
  sysLegend(id, charts[id], o.alwaysLegend);
}

function renderSystem() {
  if (!SYS.length) return;
  const themeName = document.documentElement.getAttribute('data-theme') === 'light' ? 'light' : 'dark';
  const s = SYS_COLORS[themeName];
  const muted = themeName === 'light' ? '#475569' : '#94a3b8';
  const hostIndex = Number(sysHostSelect.value) || 0;
  const {host, summary, cpu, mem, disks} = SYS[hostIndex];
  const GIB = 1048576;
  const stat = (v, decimals, unit) => v ? `${fmt(v.avg, decimals)}${unit}` : '-';
  const top = (v, decimals, unit) => v ? `max ${fmt(v.max, decimals)}${unit}` : '';
  const scaled = (v, by) => v ? {avg: v.avg / by, max: v.max / by} : null;

  document.getElementById('sys-title').textContent = SYS.length > 1 ? 'System' : `System: ${host.host}`;
  const status = document.getElementById('sys-status');
  status.hidden = !host.truncated;
  status.textContent = host.truncated
    ? `Series truncated at ${fmt(host.truncatedAtS)} s: ${host.truncated.replace(/[.\s]+$/, '')}. The figures cover the samples received.` : '';

  sysCards('sys-meta', [
    [['k', 'Host'], ['v', host.host], ['sub', host.target]],
    [['k', 'Role'], ['v', host.database ? 'database server' : 'other host']],
    [['k', 'Kernel'], ['v', host.kernel]],
    [['k', 'CPUs'], ['v', String(host.cpus)]],
    [['k', 'Memory'], ['v', `${fmt(host.memTotalKb / GIB, 1)} GiB`]],
    [['k', 'Sampling'], ['v', `every ${host.intervalS} s`], ['sub', `${fmt(host.samples)} samples`]],
  ]);

  const volumes = summary.volumes;
  const titled = (v) => v.holds.length ? `${v.name} (${v.holds.join(' + ')})` : v.name;
  const busiest = volumes.reduce((a, v) => (!a || v.busy_pct.avg > a.busy_pct.avg) ? v : a, null);
  const wal = volumes.find(v => v.holds.includes('WAL') && v.flush_ms);
  // Which disks a card is about, spelled out: every name up to three volumes.
  const written = volumes.length === 0 ? '' : volumes.length === 1 ? titled(volumes[0])
    : volumes.length <= 3 ? `all ${volumes.length} volumes: ${volumes.map(v => v.name).join(', ')}`
    : `all ${volumes.length} volumes`;
  sysCards('sys-kpis', [
    [['label', 'CPU busy'], ['value', stat(summary.cpu.busy_pct, 1, '%')], ['sub', top(summary.cpu.busy_pct, 1, '%')]],
    [['label', 'I/O wait'], ['value', stat(summary.cpu.iowait_pct, 1, '%')], ['sub', top(summary.cpu.iowait_pct, 1, '%')]],
    [['label', 'Memory used'], ['value', summary.memory.used_kb ? `${fmt(summary.memory.used_kb.max / GIB, 1)} GiB` : '-'],
     ['sub', `max, of ${fmt(host.memTotalKb / GIB, 1)} GiB`]],
    [['label', 'Disk written'], ['value', stat(summary.written_mb_s, 1, ' MB/s')],
     ['sub', top(summary.written_mb_s, 1, ' MB/s')], ['which', written]],
    [['label', 'Busiest volume'], ['value', busiest ? stat(busiest.busy_pct, 1, '%') : '-'],
     ['sub', busiest ? `busy; ${top(busiest.busy_pct, 1, '%')}` : ''], ['which', busiest ? titled(busiest) : '']],
    // How long a commit waits for the disk, when the WAL volume is known.
    wal
      ? [['label', 'WAL flush time'], ['value', stat(wal.flush_ms, 2, ' ms')], ['sub', top(wal.flush_ms, 2, ' ms')],
         ['which', wal.flush_device === wal.device ? wal.name : `${wal.flush_device}, the disk under ${wal.name}`]]
      : [['label', 'I/O pressure'], ['value', stat(summary.io_pressure_pct, 1, '%')], ['sub', top(summary.io_pressure_pct, 1, '%')]],
  ]);

  // CPU time: states at zero for the whole run are left out and named.
  const states = [['user', cpu.user, s[0]], ['system', cpu.system, s[1]], ['iowait', cpu.iowait, s[2]],
    ['softirq', cpu.softirq, s[3]], ['irq', cpu.irq, s[4]], ['nice', cpu.nice, s[5]],
    ['steal', cpu.steal, s[6]], ['guest', cpu.guest, s[7]]];
  const shown = states.filter(([, data]) => data.some(v => v > 0));
  const absent = states.filter(([, data]) => !data.some(v => v > 0)).map(([name]) => name);
  sysChart('sys-cpu', 'CPU time by state', '% of all CPUs',
    shown.map(([label, data, color]) => ({label, data, color})),
    {stacked: true, yMax: 100, unit: '%', alwaysLegend: true,
     footer: (items) => { const v = cpu.idle[items[0].dataIndex]; return v == null ? '' : `idle: ${fmt(v, 1)}%`; }});
  sysChart('sys-load', 'Run queue and load', 'Tasks', [
    {label: 'runnable', data: cpu.running, color: s[0]},
    {label: 'blocked on I/O', data: cpu.blocked, color: s[1]},
    {label: 'load average (1 min)', data: cpu.load1, color: s[2]},
    {label: `${host.cpus} CPUs`, data: DATA.timeAxis.map(() => host.cpus), color: muted, dash: [6, 4], width: 1},
  ]);
  const hasPsi = [cpu.psiCpu, cpu.psiIo, cpu.psiMem].some(series => series.some(v => v != null));
  sysChart('sys-psi', 'Pressure', '% of time stalled', [
    {label: 'CPU', data: cpu.psiCpu, color: s[0]},
    {label: 'I/O', data: cpu.psiIo, color: s[1]},
    {label: 'I/O, all tasks', data: cpu.psiIoFull, color: s[1], dash: [5, 4]},
    {label: 'memory', data: cpu.psiMem, color: s[2]},
    {label: 'memory, all tasks', data: cpu.psiMemFull, color: s[2], dash: [5, 4]},
  ], {unit: '%'});
  sysChart('sys-ctxt', 'Context switches', 'Switches/s',
    [{label: 'context switches', data: cpu.ctxt, color: s[0]}], {unit: '/s', decimals: 0});
  sysChart('sys-forks', 'Processes created', 'Processes/s',
    [{label: 'processes created', data: cpu.forks, color: s[0]}], {unit: '/s', decimals: 0});
  document.getElementById('sys-cpu-note').textContent =
    'The unfilled part up to 100% is idle time. '
    + (absent.length ? `Never above zero during this run, so not drawn: ${absent.join(', ')}. ` : '')
    + (hasPsi ? 'Pressure is the share of time some task waited for the resource; dashed lines are the time every task did.'
              : 'This host reports no pressure figures (they need Linux 4.20 or later, with pressure stall information enabled).');

  const memTotal = host.memTotalKb / GIB;
  sysChart('sys-mem', 'Memory use', 'GiB', [
    {label: 'anonymous', data: mem.anon, color: s[0]},
    {label: 'shared', data: mem.shared, color: s[1]},
    {label: 'page cache', data: mem.cache, color: s[2]},
    {label: 'slab', data: mem.slab, color: s[3]},
    {label: 'page tables', data: mem.pageTables, color: s[4]},
    {label: 'other', data: mem.other, color: s[5]},
  ], {stacked: true, yMax: Math.ceil(memTotal * 10) / 10, unit: ' GiB', decimals: 2, alwaysLegend: true,
      footer: (items) => { const v = mem.available[items[0].dataIndex]; return v == null ? '' : `available: ${fmt(v, 2)} GiB`; }});
  sysChart('sys-dirty', 'Dirty and writeback', 'MiB', [
    {label: 'dirty', data: mem.dirty, color: s[0]},
    {label: 'writeback', data: mem.writeback, color: s[1]},
  ], {unit: ' MiB', decimals: 2});
  sysChart('sys-paging', 'Paged in and out', 'MB/s', [
    {label: 'read from disk', data: mem.pgin, color: s[0]},
    {label: 'written to disk', data: mem.pgout, color: s[1]},
  ], {unit: ' MB/s'});
  sysChart('sys-faults', 'Page faults', 'Faults/s',
    [{label: 'page faults', data: mem.faults, color: s[0]}], {unit: '/s', decimals: 0});
  sysChart('sys-swap', 'Swap and major faults', 'Pages/s', [
    {label: 'swapped in', data: mem.swapIn, color: s[0]},
    {label: 'swapped out', data: mem.swapOut, color: s[1]},
    {label: 'major faults', data: mem.majFaults, color: s[2]},
  ], {unit: '/s'});
  document.getElementById('sys-mem-note').textContent =
    `The unfilled part up to ${fmt(memTotal, 1)} GiB is free memory. Shared memory holds PostgreSQL's shared buffers. `
    + 'A major fault is a page that had to be read back from disk. '
    + (mem.oomKills > 0 ? `The kernel killed ${fmt(mem.oomKills)} process(es) for lack of memory during this run.`
                        : 'The kernel killed no process for lack of memory during this run.');

  // Disks: the table of volumes, then the charts of the device picked.
  const diskBox = document.getElementById('sys-disk');
  diskBox.hidden = disks.length === 0;
  if (!disks.length) return;
  const body = document.getElementById('sys-volumes-body');
  body.replaceChildren();
  for (const v of volumes) {
    const tr = document.createElement('tr');
    [v.name, v.holds.join(' + ') || '-', v.on.join(', on ') || '-',
     fmt(v.written_mb_s.avg, 1), fmt(v.read_mb_s.avg, 1), fmt(v.requests_s.avg),
     fmt(v.write_ms, 2), v.flush_ms ? fmt(v.flush_ms.avg, 2) : '-',
     fmt(v.busy_pct.avg, 1), fmt(v.busy_pct.max, 1),
    ].forEach(text => { const td = document.createElement('td'); td.textContent = text; tr.appendChild(td); });
    body.appendChild(tr);
  }
  // A new host brings its own devices: the selector starts again on the first.
  if (hostIndex !== sysShownHost) {
    sysShownHost = hostIndex;
    sysDeviceSelect.replaceChildren();
    disks.forEach((d, i) => sysDeviceSelect.add(new Option(d.top ? d.title : `${d.name} (lower layer)`, String(i))));
  }
  const d = disks[Number(sysDeviceSelect.value) || 0];
  const flushLabel = d.flushName ? `flush (${d.flushName})` : 'flush';
  sysChart('sys-dthr', `Throughput of ${d.title}`, 'MB/s', [
    {label: 'read', data: d.rmb, color: s[0]},
    {label: 'write', data: d.wmb, color: s[1]},
  ], {unit: ' MB/s'});
  const requests = [{label: 'read', data: d.r, color: s[0]}, {label: 'write', data: d.w, color: s[1]}];
  if (d.f) requests.push({label: flushLabel, data: d.f, color: s[2]});
  sysChart('sys-diops', `Requests of ${d.title}`, 'Requests/s', requests, {unit: '/s', decimals: 0});
  const waits = [{label: 'read', data: d.rAwait, color: s[0]}, {label: 'write', data: d.wAwait, color: s[1]}];
  if (d.fAwait) waits.push({label: flushLabel, data: d.fAwait, color: s[2]});
  sysChart('sys-dawait', `Request time of ${d.title}`, 'ms per request, queue included', waits, {unit: ' ms', decimals: 2});
  sysChart('sys-dsize', `Request size of ${d.title}`, 'kB per request', [
    {label: 'read', data: d.rSize, color: s[0]},
    {label: 'write', data: d.wSize, color: s[1]},
  ], {unit: ' kB'});
  sysChart('sys-dqueue', `Queue size of ${d.title}`, 'Requests waiting or in progress',
    [{label: 'queue size', data: d.queue, color: s[0]}], {decimals: 2});
  const tops = disks.filter(x => x.top).slice(0, 8);
  sysChart('sys-dutil', 'Time busy, all volumes', '% of time',
    tops.map((x, i) => ({label: x.title, data: x.util, color: s[i]})), {unit: '%', yMax: 100, alwaysLegend: true});
  const chains = volumes.map(v => [v.name, ...v.on].join(', on '));
  document.getElementById('sys-disk-note').textContent =
    'The first five charts follow the device picked above; the last one compares the top of each stack, the devices file systems sit on'
    + (chains.length ? `: ${chains.join('; ')}. ` : '. ')
    + 'The layers underneath carry the same I/O and are in the selector. '
    + (d.flushName ? `Flushes are counted at the disk, so the flush lines are those of ${d.flushName}: every volume on that disk shares them. ` : '')
    + (summary.idle_volumes.length ? `Never 1% busy during this run, so not shown: ${summary.idle_volumes.join(', ')}.` : '');
}

if (SYS.length) {
  document.getElementById('system-view').hidden = false;
  SYS.forEach((h, i) => sysHostSelect.add(new Option(
    `${h.host.host} (${h.host.target})${h.host.database ? ' - database server' : ''}`, String(i))));
  // The section opens on the host PostgreSQL runs on, wherever it is in the list.
  sysHostSelect.value = String(Math.max(0, SYS.findIndex(h => h.host.database)));
  document.getElementById('sys-host-label').hidden = SYS.length < 2;
  sysHostSelect.addEventListener('change', renderSystem);
  sysDeviceSelect.addEventListener('change', renderSystem);
  renderSystem();
}
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::sysmon::delta::{CpuShares, MemorySample, MEM_GAUGES, VM_RATES};
    use crate::executor::sysmon::HostInfo;

    fn disk(device: &str, wkb_s: f64, flushes: Option<f64>) -> DiskSample {
        DiskSample {
            device: device.to_string(),
            w_s: 100.0,
            wkb_s,
            util_pct: 50.0,
            f_s: flushes,
            f_await_ms: flushes.map(|_| 2.0),
            ..DiskSample::default()
        }
    }

    fn sample(timestamp: f64, user: f64, wkb_s: f64) -> TimedSample {
        let mut gauges = vec![None; MEM_GAUGES.len()];
        for (field, kb) in [
            ("MemTotal", 4 * 1_048_576),
            ("MemFree", 1_048_576),
            ("AnonPages", 1_048_576),
            ("Cached", 1_048_576),
            ("Buffers", 0),
            ("Shmem", 524_288),
            ("Slab", 0),
            ("PageTables", 0),
        ] {
            gauges[MEM_GAUGES.iter().position(|f| *f == field).unwrap()] = Some(kb);
        }
        TimedSample {
            timestamp,
            sample: Sample {
                interval_s: 1.0,
                cpu: Some(CpuShares {
                    user,
                    idle: 100.0 - user,
                    ..CpuShares::default()
                }),
                memory: MemorySample {
                    gauges,
                    rates: vec![Some(0.0); VM_RATES.len()],
                },
                disks: vec![
                    disk("dm-0", wkb_s, Some(0.0)),
                    disk("nvme0n1", wkb_s, Some(40.0)),
                ],
                ..Sample::default()
            },
        }
    }

    fn host(samples: Vec<TimedSample>, truncated: Option<&str>) -> HostSeries {
        let device =
            |name: &str, mapper: Option<&str>, on: &[&str], top: bool, holds: &[&str]| Device {
                name: name.to_string(),
                mapper_name: mapper.map(str::to_string),
                on: on.iter().map(|s| s.to_string()).collect(),
                top,
                holds: holds.iter().map(|s| s.to_string()).collect(),
            };
        HostSeries {
            info: HostInfo {
                target: "local".into(),
                label: "local".into(),
                host: "db1".into(),
                kernel: "6.1".into(),
                cpus: 4,
                mem_total_kb: 4 * 1_048_576,
                interval_s: 1.0,
                database: true,
                devices: vec![
                    device(
                        "dm-0",
                        Some("vg-data"),
                        &["nvme0n1"],
                        true,
                        &["data", "WAL"],
                    ),
                    device("nvme0n1", None, &[], false, &[]),
                ],
            },
            samples,
            truncated: truncated.map(str::to_string),
        }
    }

    #[test]
    fn samples_land_in_the_buckets_of_the_time_axis() {
        // Two-second buckets from second 1000; the sampler died after 1004.
        let series = host(
            vec![
                sample(1000.4, 10.0, 1024.0),
                sample(1001.4, 30.0, 3072.0),
                sample(1002.4, 50.0, 2048.0),
                sample(1004.4, 70.0, 4096.0),
            ],
            Some("the sampler ended"),
        );
        let system = system_json(&[series], Some(1001), Some(1000), &[0, 2, 4, 6]);
        let host = &system[0];
        // The mean of a bucket's samples, null where there is none.
        assert_eq!(host["cpu"]["user"], json!([20.0, 50.0, 70.0, null]));
        assert_eq!(host["cpu"]["idle"], json!([80.0, 50.0, 30.0, null]));
        // 1 GiB anonymous, 0.5 shared, 0.5 of page cache, and what is left
        // of the 4 once the free GiB is counted.
        assert_eq!(host["mem"]["anon"][0], 1.0);
        assert_eq!(host["mem"]["cache"][0], 0.5);
        assert_eq!(host["mem"]["other"][0], 1.0);
        assert_eq!(host["host"]["truncated"], "the sampler ended");
        assert_eq!(host["host"]["truncatedAtS"], 4.0);
        assert_eq!(host["host"]["database"], true);
        // The cards' figures are the window's: the two samples whose second
        // began after the ramp-up ended at 1001.
        assert_eq!(host["summary"]["samples"], 2);
        assert_eq!(host["summary"]["cpu"]["busy_pct"]["avg"], 60.0);
    }

    #[test]
    fn a_volume_comes_with_the_flushes_of_its_disk_and_its_lower_layers() {
        let series = host(vec![sample(1000.5, 10.0, 2048.0)], None);
        let system = system_json(&[series], None, Some(1000), &[0]);
        let disks = system[0]["disks"].as_array().unwrap();
        assert_eq!(disks.len(), 2);
        let volume = &disks[0];
        assert_eq!(volume["title"], "vg-data (data + WAL)");
        assert_eq!(volume["top"], true);
        assert_eq!(volume["wmb"], json!([2.0]));
        // Counted on the disk, drawn on the volume, and said so.
        assert_eq!(volume["flushName"], "nvme0n1");
        assert_eq!(volume["f"], json!([40.0]));
        assert_eq!(volume["fAwait"], json!([2.0]));
        let lower = &disks[1];
        assert_eq!(lower["device"], "nvme0n1");
        assert_eq!(lower["top"], false);
        assert!(lower["flushName"].is_null());
        assert_eq!(lower["f"], json!([40.0]));
    }

    #[test]
    fn no_time_axis_or_no_host_means_no_section() {
        let series = host(vec![sample(1000.5, 10.0, 1.0)], None);
        assert_eq!(system_json(&[series], None, None, &[]), json!([]));
        assert_eq!(system_json(&[], None, Some(1000), &[0]), json!([]));
    }
}
