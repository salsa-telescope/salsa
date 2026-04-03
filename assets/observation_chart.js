// --- Module-level analysis state ---
let analysisState = null;
let chartRefs = null;

// --- Math helpers ---

function solveLinear(A, b) {
  const n = b.length;
  const M = A.map((row, i) => [...row, b[i]]);
  for (let col = 0; col < n; col++) {
    let maxRow = col;
    for (let row = col + 1; row < n; row++) {
      if (Math.abs(M[row][col]) > Math.abs(M[maxRow][col])) maxRow = row;
    }
    [M[col], M[maxRow]] = [M[maxRow], M[col]];
    if (Math.abs(M[col][col]) < 1e-15) return null;
    for (let row = 0; row < n; row++) {
      if (row === col) continue;
      const factor = M[row][col] / M[col][col];
      for (let j = col; j <= n; j++) M[row][j] -= factor * M[col][j];
    }
  }
  return M.map((row, i) => row[n] / row[i]);
}

// Polynomial least-squares fit. Returns coefficients [a0, a1, ...] (lowest degree first).
function polyFit(xs, ys, degree) {
  const m = degree + 1;
  const A = Array.from({ length: m }, () => new Array(m).fill(0));
  const b = new Array(m).fill(0);
  for (let i = 0; i < xs.length; i++) {
    const powers = Array.from({ length: m }, (_, k) => Math.pow(xs[i], k));
    for (let j = 0; j < m; j++) {
      b[j] += powers[j] * ys[i];
      for (let l = 0; l < m; l++) A[j][l] += powers[j] * powers[l];
    }
  }
  return solveLinear(A, b);
}

function evalPoly(coeffs, x) {
  return coeffs.reduce((sum, c, k) => sum + c * Math.pow(x, k), 0);
}

// Multi-Gaussian Levenberg–Marquardt fit.
// seeds: [{amplitude, center, sigma}] in display units
// Returns [{amplitude, center, sigma, fwhm}]
function lmFitGaussians(xs, ys, seeds) {
  let params = seeds.flatMap((s) => [s.amplitude, s.center, s.sigma]);
  const n = xs.length;
  const m = params.length;

  function model(p, x) {
    let sum = 0;
    for (let k = 0; k < p.length; k += 3) {
      const dx = x - p[k + 1];
      sum += p[k] * Math.exp((-dx * dx) / (2 * p[k + 2] * p[k + 2]));
    }
    return sum;
  }

  function residuals(p) {
    return xs.map((x, i) => ys[i] - model(p, x));
  }

  function jacobian(p) {
    return xs.map((x) => {
      const row = new Array(m).fill(0);
      for (let k = 0; k < p.length; k += 3) {
        const A = p[k], mu = p[k + 1], sig = p[k + 2];
        const dx = x - mu;
        const g = Math.exp((-dx * dx) / (2 * sig * sig));
        row[k] = -g;
        row[k + 1] = -A * g * dx / (sig * sig);
        row[k + 2] = -A * g * dx * dx / (sig * sig * sig);
      }
      return row;
    });
  }

  let lambda = 1e-3;
  for (let iter = 0; iter < 200; iter++) {
    const r = residuals(params);
    const J = jacobian(params);
    const JtJ = Array.from({ length: m }, () => new Array(m).fill(0));
    const Jtr = new Array(m).fill(0);
    for (let i = 0; i < n; i++) {
      for (let j = 0; j < m; j++) {
        Jtr[j] -= J[i][j] * r[i];
        for (let l = 0; l < m; l++) JtJ[j][l] += J[i][j] * J[i][l];
      }
    }
    const Aug = JtJ.map((row, j) =>
      row.map((v, l) => (j === l ? v * (1 + lambda) : v))
    );
    const delta = solveLinear(Aug, Jtr);
    if (!delta) break;
    const newParams = params.map((p, j) => p + delta[j]);
    const oldCost = r.reduce((s, v) => s + v * v, 0);
    const newCost = residuals(newParams).reduce((s, v) => s + v * v, 0);
    if (newCost < oldCost) {
      params = newParams;
      lambda /= 10;
      if (Math.sqrt(delta.reduce((s, v) => s + v * v, 0)) < 1e-10) break;
    } else {
      lambda *= 10;
      if (lambda > 1e10) break;
    }
  }

  return Array.from({ length: params.length / 3 }, (_, k) => ({
    amplitude: params[k * 3],
    center: params[k * 3 + 1],
    sigma: Math.abs(params[k * 3 + 2]),
    fwhm: Math.abs(params[k * 3 + 2]) * 2 * Math.sqrt(2 * Math.log(2)),
  }));
}

// --- Analysis UI functions (called from HTML) ---

function togglePickMode(mode) {
  if (!analysisState) return;
  if (analysisState.clickMode === mode) {
    analysisState.clickMode = null;
  } else {
    analysisState.clickMode = mode;
  }
  updateAnalysisUI();
}

function updateAnalysisUI() {
  if (!analysisState) return;
  const mode = analysisState.clickMode;

  const btnBaseline = document.getElementById("btn-pick-baseline");
  const btnGaussian = document.getElementById("btn-pick-gaussian");
  const btnFit = document.getElementById("btn-fit-baseline");
  const hint = document.getElementById("chart-hint");

  if (btnBaseline) {
    btnBaseline.classList.toggle("bg-orange-200", mode === "baseline");
    btnBaseline.classList.toggle("bg-gray-200", mode !== "baseline");
    btnBaseline.textContent = mode === "baseline" ? "Done picking" : "Pick ranges";
  }
  if (btnGaussian) {
    btnGaussian.classList.toggle("bg-orange-200", mode === "gaussian");
    btnGaussian.classList.toggle("bg-gray-200", mode !== "gaussian");
    btnGaussian.textContent = mode === "gaussian" ? "Done picking" : "Pick peaks";
  }
  if (btnFit) {
    btnFit.disabled = analysisState.baselineRanges.length === 0 || mode === "baseline";
  }
  const btnFitGaussian = document.getElementById("btn-fit-gaussian");
  if (btnFitGaussian) {
    btnFitGaussian.disabled = analysisState.gaussianSeeds.length === 0 || mode === "gaussian";
  }
  if (hint) {
    const pending = analysisState.pendingRangeStart !== null;
    hint.textContent =
      mode === "baseline"
        ? pending
          ? "Click to set the end of this range"
          : "Click to start a baseline range · click Done when finished"
        : mode === "gaussian"
        ? "Click on peak centers to add Gaussian seeds · click Done when finished"
        : "Hover to show coordinates · draw a box to zoom · double-click to reset";
  }

  const baselineCount = document.getElementById("baseline-count");
  if (baselineCount) {
    const n = analysisState.baselineRanges.length;
    const pending = analysisState.pendingRangeStart !== null ? " (picking…)" : "";
    baselineCount.textContent = `${n} range${n !== 1 ? "s" : ""}${pending}`;
  }

  const gaussianCount = document.getElementById("gaussian-count");
  if (gaussianCount)
    gaussianCount.textContent = `${analysisState.gaussianSeeds.length} seed${analysisState.gaussianSeeds.length !== 1 ? "s" : ""}`;

  // Enable/disable brush based on pick mode
  if (chartRefs) {
    chartRefs.brushG.select("rect.overlay").style("pointer-events", mode ? "none" : null);
    chartRefs.brushG.select("rect.selection").style("pointer-events", mode ? "none" : null);
    chartRefs.clickOverlay.style("pointer-events", mode ? null : "none");
  }
}

function fitAndSubtractBaseline() {
  if (!analysisState || analysisState.baselineRanges.length === 0) return;
  const { freqsHz, correctedAmps } = analysisState;
  const degree = parseInt(document.getElementById("baseline-order").value, 10);

  // Collect data points that fall within any baseline range
  const xs = [], ys = [];
  freqsHz.forEach((f, i) => {
    const xd = chartRefs.freqToDisplay(f);
    for (const r of analysisState.baselineRanges) {
      if (xd >= r.x0 && xd <= r.x1) {
        xs.push(xd);
        ys.push(correctedAmps[i]);
        break;
      }
    }
  });

  if (xs.length < degree + 1) {
    alert("Not enough data points in selected ranges for this polynomial order.");
    return;
  }
  const coeffs = polyFit(xs, ys, degree);
  if (!coeffs) {
    alert("Baseline fit failed (singular matrix).");
    return;
  }
  analysisState.baselineCoeffs = coeffs;
  analysisState.correctedAmps = correctedAmps.map((amp, i) => {
    const xd = chartRefs.freqToDisplay(freqsHz[i]);
    return amp - evalPoly(coeffs, xd);
  });
  analysisState.baselineRanges = [];
  analysisState.pendingRangeStart = null;
  chartRefs.rescaleAndRedraw();
  updateAnalysisUI();
  updateCsvLink();
}

function clearBaseline() {
  if (!analysisState) return;
  analysisState.baselineRanges = [];
  analysisState.pendingRangeStart = null;
  updateAnalysisUI();
  updateOverlays();
}

function fitGaussians() {
  if (!analysisState || analysisState.gaussianSeeds.length === 0) {
    alert("Pick at least one peak seed first.");
    return;
  }
  const { freqsHz, correctedAmps } = analysisState;
  const xs = freqsHz.map((f) => chartRefs.freqToDisplay(f));
  const xRange = xs[xs.length - 1] - xs[0];
  const defaultSigma = Math.abs(xRange) / 20;

  const seeds = analysisState.gaussianSeeds.map((s) => ({
    amplitude: s.y,
    center: s.xDisplay,
    sigma: defaultSigma,
  }));

  try {
    const fits = lmFitGaussians(xs, correctedAmps, seeds);
    analysisState.gaussianFits = fits;
    updateOverlays();
    renderGaussianResults(fits);
  } catch (e) {
    alert("Gaussian fit failed: " + e.message);
  }
}

function renderGaussianResults(fits) {
  const el = document.getElementById("gaussian-results");
  if (!el) return;
  const unit = chartRefs ? chartRefs.xUnit() : "MHz";
  el.innerHTML = fits
    .map(
      (f, i) =>
        `<div class="border-t pt-1 mt-1">` +
        `<span class="font-semibold">G${i + 1}</span><br>` +
        `Ctr: ${f.center.toFixed(2)} ${unit}<br>` +
        `Amp: ${f.amplitude.toFixed(3)}<br>` +
        `FWHM: ${f.fwhm.toFixed(2)} ${unit}` +
        `</div>`
    )
    .join("");
}

function clearGaussians() {
  if (!analysisState) return;
  analysisState.gaussianSeeds = [];
  analysisState.gaussianFits = [];
  document.getElementById("gaussian-results").innerHTML = "";
  updateAnalysisUI();
  updateOverlays();
}

function resetAnalysis() {
  if (!analysisState) return;
  analysisState.correctedAmps = analysisState.rawAmps.slice();
  analysisState.baselineRanges = [];
  analysisState.pendingRangeStart = null;
  analysisState.gaussianSeeds = [];
  analysisState.gaussianFits = [];
  analysisState.baselineCoeffs = null;
  analysisState.clickMode = null;
  document.getElementById("gaussian-results").innerHTML = "";
  chartRefs.rescaleAndRedraw();
  updateAnalysisUI();
  updateOverlays();
  updateCsvLink();
}

function updateOverlays() {
  if (!chartRefs || !analysisState) return;
  const { x, y, baselineDotsG, baselineCurveG, gaussianDotsG, gaussianCurvesG,
          freqToDisplay, xUnit } = chartRefs;

  // Baseline range shading
  baselineDotsG.selectAll("*").remove();
  // Complete ranges: shaded rects
  baselineDotsG.selectAll("rect")
    .data(analysisState.baselineRanges)
    .join("rect")
    .attr("x", (d) => x(d.x0))
    .attr("y", (r) => { const [, yMax] = y.domain(); return y(yMax); })
    .attr("width", (d) => Math.max(0, x(d.x1) - x(d.x0)))
    .attr("height", (r) => { const [yMin, yMax] = y.domain(); return y(yMin) - y(yMax); })
    .attr("fill", "orange")
    .attr("opacity", 0.2)
    .attr("clip-path", "url(#plot-clip)");
  // Pending range start: vertical dashed line
  if (analysisState.pendingRangeStart !== null) {
    const px = x(analysisState.pendingRangeStart);
    const [yMin, yMax] = y.domain();
    baselineDotsG.append("line")
      .attr("x1", px).attr("x2", px)
      .attr("y1", y(yMax)).attr("y2", y(yMin))
      .attr("stroke", "darkorange")
      .attr("stroke-width", 1.5)
      .attr("stroke-dasharray", "4,3")
      .attr("clip-path", "url(#plot-clip)");
  }

  // Fitted baseline curve
  baselineCurveG.selectAll("path").remove();
  if (analysisState.baselineCoeffs) {
    const { freqsHz } = analysisState;
    const curvePoints = freqsHz.map((f) => {
      const xd = freqToDisplay(f);
      return { x: xd, y: evalPoly(analysisState.baselineCoeffs, xd) };
    });
    const lineFn = d3.line().x((d) => x(d.x)).y((d) => y(d.y));
    baselineCurveG.append("path")
      .datum(curvePoints)
      .attr("fill", "none")
      .attr("stroke", "orange")
      .attr("stroke-width", 1.5)
      .attr("stroke-dasharray", "4,3")
      .attr("clip-path", "url(#plot-clip)")
      .attr("d", lineFn);
  }

  // Gaussian seed dots
  gaussianDotsG.selectAll("circle").remove();
  gaussianDotsG.selectAll("circle")
    .data(analysisState.gaussianSeeds)
    .join("circle")
    .attr("cx", (d) => x(d.xDisplay))
    .attr("cy", (d) => y(d.y))
    .attr("r", 4)
    .attr("fill", "crimson")
    .attr("stroke", "darkred")
    .attr("stroke-width", 1)
    .attr("clip-path", "url(#plot-clip)");

  // Fitted Gaussian curves
  gaussianCurvesG.selectAll("path").remove();
  if (analysisState.gaussianFits.length > 0) {
    const { freqsHz } = analysisState;
    const xs = freqsHz.map((f) => freqToDisplay(f));
    const colors = ["crimson", "darkorchid", "teal", "chocolate", "steelblue"];

    analysisState.gaussianFits.forEach((fit, idx) => {
      const curvePoints = xs.map((xd) => {
        const dx = xd - fit.center;
        return { x: xd, y: fit.amplitude * Math.exp((-dx * dx) / (2 * fit.sigma * fit.sigma)) };
      });
      const lineFn = d3.line().x((d) => x(d.x)).y((d) => y(d.y));
      gaussianCurvesG.append("path")
        .datum(curvePoints)
        .attr("fill", "none")
        .attr("stroke", colors[idx % colors.length])
        .attr("stroke-width", 2)
        .attr("clip-path", "url(#plot-clip)")
        .attr("d", lineFn);
    });
  }
}

function updateCsvLink() {
  if (!analysisState || !chartRefs) return;
  const { freqsHz } = analysisState;
  const xHeader = chartRefs.xUnit() === "km/s" ? "vlsr_km_s" : "frequency_hz";
  const rows = [`${xHeader},amplitude`, ...freqsHz.map((f, i) => `${chartRefs.freqToDisplay(f)},${analysisState.correctedAmps[i]}`)];
  const blob = new Blob([rows.join("\n")], { type: "text/csv" });
  const dlCsv = document.getElementById("download-csv");
  if (dlCsv && dlCsv.dataset.originalHref) {
    // Use server href only when showing raw frequency with no baseline subtraction
    const isReset = analysisState.correctedAmps.every((v, i) => v === analysisState.rawAmps[i]);
    if (isReset && xHeader === "frequency_hz") {
      dlCsv.href = dlCsv.dataset.originalHref;
    } else {
      dlCsv.href = URL.createObjectURL(blob);
    }
  }
}

// --- PNG export ---

function exportPng(id, telescopeId, startTime) {
  const svg = document.querySelector("#observation-chart svg");
  if (!svg) return;
  const scale = 2;
  const width = parseInt(svg.getAttribute("width")) || 640;
  const height = parseInt(svg.getAttribute("height")) || 420;
  const svgData = new XMLSerializer().serializeToString(svg);
  const canvas = document.createElement("canvas");
  canvas.width = width * scale;
  canvas.height = height * scale;
  const ctx = canvas.getContext("2d");
  const img = new Image();
  img.onload = () => {
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.scale(scale, scale);
    ctx.drawImage(img, 0, 0, width, height);
    const tag = new Date(startTime)
      .toISOString()
      .slice(0, 19)
      .replace(/[-:]/g, "")
      .replace("T", "T");
    const a = document.createElement("a");
    a.download = `SALSA-${telescopeId}-${tag}.png`;
    a.href = canvas.toDataURL("image/png");
    a.click();
  };
  img.src =
    "data:image/svg+xml;charset=utf-8," + encodeURIComponent(svgData);
}

function autoLoadFirstObservation() {
  const chart = document.getElementById("observation-chart");
  const firstRow = document.querySelector("[id^='obs-row-']");
  if (firstRow && chart && chart.style.display === "none") {
    const id = parseInt(firstRow.id.replace("obs-row-", ""));
    loadObservation(id);
  }
}

document.addEventListener("htmx:afterSettle", autoLoadFirstObservation);
document.addEventListener("DOMContentLoaded", autoLoadFirstObservation);

function loadObservation(id) {
  const C = 299792458; // m/s
  const F_REST = 1420.405751e6; // Hz

  fetch(`/observations/${id}`)
    .then((res) => {
      if (!res.ok) throw new Error("Failed to fetch observation");
      return res.json();
    })
    .then((data) => {
      // Highlight selected observation row, show only its delete button
      document.querySelectorAll("[id^='obs-row-']").forEach((el) => {
        el.classList.remove("bg-indigo-50", "border-indigo-300");
        el.classList.add("border-transparent");
      });
      document.querySelectorAll("[id^='del-btn-']").forEach((el) => {
        el.classList.add("hidden");
      });
      const selectedRow = document.getElementById(`obs-row-${id}`);
      if (selectedRow) {
        selectedRow.classList.remove("border-transparent");
        selectedRow.classList.add("bg-indigo-50", "border-indigo-300");
      }
      const delBtn = document.getElementById(`del-btn-${id}`);
      if (delBtn) {
        delBtn.classList.remove("hidden");
      }

      const container = document.getElementById("observation-chart");
      container.style.display = "block";

      // Init analysis state for this observation
      analysisState = {
        rawAmps: data.amplitudes.slice(),
        correctedAmps: data.amplitudes.slice(),
        freqsHz: data.frequencies.slice(),
        baselineRanges: [],
        pendingRangeStart: null,
        gaussianSeeds: [],
        gaussianFits: [],
        baselineCoeffs: null,
        clickMode: null,
      };
      chartRefs = null;

      // Wire up download buttons
      const downloads = document.getElementById("observation-downloads");
      if (downloads) downloads.classList.remove("hidden");
      const dlCsv = document.getElementById("download-csv");
      if (dlCsv) {
        dlCsv.href = `/observations/${id}/csv`;
        dlCsv.dataset.originalHref = `/observations/${id}/csv`;
      }
      const dlFits = document.getElementById("download-fits");
      if (dlFits) dlFits.href = `/observations/${id}/fits`;
      const dlPng = document.getElementById("download-png");
      if (dlPng) dlPng.onclick = () => exportPng(id, data.telescope_id, data.start_time);

      // Remove any previous SVG
      const existing = container.querySelector("svg");
      if (existing) existing.remove();

      const width = 640;
      const height = 420;
      const margin = 60;

      // Compute power level: average of center 50% of amplitudes
      const amps = data.amplitudes;
      const pLo = Math.floor(amps.length * 0.25);
      const pHi = Math.ceil(amps.length * 0.75);
      const centerAmps = amps.slice(pLo, pHi);
      const powerLevel = centerAmps.reduce((s, v) => s + v, 0) / centerAmps.length;

      const startTime = new Date(data.start_time).toUTCString();
      const intTime = Math.round(data.integration_time_secs);
      const coordLabel = data.coordinate_system === "sun" ? "Sun az/el" : data.coordinate_system;
      const coordStr = `(${data.target_x.toFixed(1)}°, ${data.target_y.toFixed(1)}°)`;
      const offsetParts = [];
      const azOff = data.az_offset_deg;
      const elOff = data.el_offset_deg;
      if (azOff && Math.abs(azOff) >= 0.05) offsetParts.push(`az ${azOff >= 0 ? "+" : ""}${azOff.toFixed(1)}°`);
      if (elOff && Math.abs(elOff) >= 0.05) offsetParts.push(`el ${elOff >= 0 ? "+" : ""}${elOff.toFixed(1)}°`);
      const offsetStr = offsetParts.length > 0 ? ` + offset ${offsetParts.join(", ")}` : "";
      const titleLine1 = `${data.telescope_id} — ${coordLabel} ${coordStr}${offsetStr} — ${intTime}s  |  Avg. power: ${powerLevel.toFixed(2)}`;
      const titleLine2 = startTime;

      const vlsrCorrection = data.vlsr_correction_mps;
      let showVlsr = vlsrCorrection !== null && vlsrCorrection !== undefined;
      let showLog = false;

      function freqToVlsr(freqHz) {
        return (C * (F_REST - freqHz) / F_REST + vlsrCorrection) / 1000;
      }

      function freqToDisplay(freqHz) {
        return showVlsr ? freqToVlsr(freqHz) : freqHz / 1e6;
      }

      function xUnit() {
        return showVlsr ? "km/s" : "MHz";
      }

      function getDisplayData() {
        const corrected = analysisState ? analysisState.correctedAmps : data.amplitudes;
        return data.frequencies.map((f, i) => ({
          x: freqToDisplay(f),
          y: corrected[i],
        }));
      }

      const points = getDisplayData();

      const xExtent = d3.extent(points, (d) => d.x);
      const yExtent = d3.extent(points, (d) => d.y);
      const yPadding = (yExtent[1] - yExtent[0]) * 0.05;

      let fullXDomain = xExtent.slice();
      let fullYDomain = [yExtent[0] - yPadding, yExtent[1] + yPadding];

      const x = d3.scaleLinear().domain(fullXDomain).range([margin, width - margin]);
      let y = makeYScale(fullYDomain);

      const svg = d3
        .create("svg")
        .attr("width", width)
        .attr("height", height)
        .attr("viewBox", [0, 0, width, height])
        .attr("style", "max-width: 100%; height: auto;");

      // Chart title (also included when exporting as PNG)
      const titleGroup = svg.append("text")
        .attr("x", width / 2)
        .attr("text-anchor", "middle")
        .attr("font-size", "12px")
        .attr("fill", "#6b7280");
      titleGroup.append("tspan")
        .attr("x", width / 2)
        .attr("dy", "1em")
        .text(titleLine1);
      titleGroup.append("tspan")
        .attr("x", width / 2)
        .attr("dy", "1.2em")
        .text(titleLine2);

      // Clip path to keep line inside plot area when zoomed
      svg.append("defs")
        .append("clipPath")
        .attr("id", "plot-clip")
        .append("rect")
        .attr("x", margin)
        .attr("y", margin)
        .attr("width", width - 2 * margin)
        .attr("height", height - 2 * margin);

      // x-axis
      const xAxisG = svg
        .append("g")
        .attr("transform", `translate(0,${height - margin})`)
        .call(d3.axisBottom(x).ticks(width / 160).tickSizeOuter(0))
        .call((g) => g.selectAll("text").attr("font-size", "13px"));

      // x-axis label
      const xLabel = svg
        .append("text")
        .attr("x", width / 2)
        .attr("y", height - 8)
        .attr("text-anchor", "middle")
        .attr("font-size", "15px")
        .text(showVlsr ? "VLSR (km/s)" : "Frequency (MHz)");

      // y-axis
      const yAxisG = svg
        .append("g")
        .attr("transform", `translate(${margin},0)`)
        .call(showLog ? d3.axisLeft(y).ticks(6, ".2~e") : d3.axisLeft(y).ticks(height / 80))
        .call((g) => g.selectAll("text").attr("font-size", "13px"));

      // y-axis label
      svg
        .append("text")
        .attr("transform", `rotate(-90)`)
        .attr("x", -(height / 2))
        .attr("y", 15)
        .attr("text-anchor", "middle")
        .attr("font-size", "15px")
        .attr("fill", "black")
        .text("Amplitude");

      // Line
      const lineFn = d3
        .line()
        .x((d) => x(d.x))
        .y((d) => y(d.y));

      const initialPoints = showLog
        ? points.map((d) => ({ x: d.x, y: Math.max(d.y, y.domain()[0]) }))
        : points;
      const path = svg
        .append("path")
        .datum(initialPoints)
        .attr("fill", "none")
        .attr("stroke", "steelblue")
        .attr("stroke-width", 1.5)
        .attr("clip-path", "url(#plot-clip)")
        .attr("d", lineFn);

      // Overlay groups (drawn above line, below tooltip)
      const baselineDotsG = svg.append("g").attr("class", "baseline-dots");
      const baselineCurveG = svg.append("g").attr("class", "baseline-curve");
      const gaussianDotsG = svg.append("g").attr("class", "gaussian-dots");
      const gaussianCurvesG = svg.append("g").attr("class", "gaussian-curves");

      // Tooltip
      const tooltip = svg
        .append("text")
        .attr("x", width - margin)
        .attr("y", margin)
        .attr("text-anchor", "end")
        .attr("alignment-baseline", "hanging")
        .attr("font-size", "12px")
        .attr("fill", "black");

      // Zoom helpers
      function visibleYDomain(currentData, xDom) {
        const visible = currentData.filter((d) => d.x >= xDom[0] && d.x <= xDom[1]);
        if (visible.length === 0) return fullYDomain;
        const yMin = d3.min(visible, (d) => d.y);
        const yMax = d3.max(visible, (d) => d.y);
        const pad = (yMax - yMin) * 0.05 || 1;
        return [yMin - pad, yMax + pad];
      }

      function makeYScale(yDom) {
        if (showLog) {
          const yMax = yDom[1];
          const yFloor = Math.max(yMax * 1e-4, 1e-6);
          return d3.scaleLog().domain([yFloor, yMax]).range([height - margin, margin]).clamp(true);
        }
        return d3.scaleLinear().domain(yDom).nice().range([height - margin, margin]);
      }

      function redraw(currentData, xDom, yDom) {
        x.domain(xDom);
        y = makeYScale(yDom);
        if (chartRefs) chartRefs.y = y;
        xAxisG.call(d3.axisBottom(x).ticks(width / 160).tickSizeOuter(0))
          .call((g) => g.selectAll("text").attr("font-size", "13px"));
        yAxisG.call(showLog
          ? d3.axisLeft(y).ticks(6, ".2~e")
          : d3.axisLeft(y).ticks(height / 80))
          .call((g) => g.selectAll("text").attr("font-size", "13px"));
        const drawData = showLog
          ? currentData.map((d) => ({ x: d.x, y: Math.max(d.y, y.domain()[0]) }))
          : currentData;
        path.datum(drawData).attr("d", lineFn);
        updateOverlays();
      }

      function refreshLine() {
        const currentData = getDisplayData();
        path.datum(currentData).attr("d", lineFn);
      }

      function rescaleAndRedraw() {
        const currentData = getDisplayData();
        const yExtentNew = d3.extent(currentData, (d) => d.y);
        const yPad = (yExtentNew[1] - yExtentNew[0]) * 0.05 || 1;
        fullYDomain = [yExtentNew[0] - yPad, yExtentNew[1] + yPad];
        redraw(currentData, fullXDomain, fullYDomain);
      }

      // Brush for zoom
      const brush = d3.brush()
        .extent([[margin, margin], [width - margin, height - margin]])
        .on("end", function (event) {
          if (!event.selection) return;
          const [[px0, py0], [px1, py1]] = event.selection;
          const x0 = x.invert(px0), x1 = x.invert(px1);
          const y0 = y.invert(py1), y1 = y.invert(py0);
          const currentData = getDisplayData();
          redraw(currentData, [x0, x1], [y0, y1]);
          brushG.call(brush.move, null);
        });

      const brushG = svg.append("g")
        .attr("class", "brush")
        .call(brush);

      // Transparent click overlay for pick mode (covers plot area, disabled when not in pick mode)
      const clickOverlay = svg.append("rect")
        .attr("x", margin)
        .attr("y", margin)
        .attr("width", width - 2 * margin)
        .attr("height", height - 2 * margin)
        .attr("fill", "transparent")
        .style("pointer-events", "none")
        .on("click", function (event) {
          if (!analysisState || !analysisState.clickMode) return;
          const [mouseX, mouseY] = d3.pointer(event);
          const xDisplay = x.invert(mouseX);
          const yVal = y.invert(mouseY);
          const freqHz = showVlsr
            ? F_REST - (xDisplay * 1000 - vlsrCorrection) * F_REST / C
            : xDisplay * 1e6;
          if (analysisState.clickMode === "baseline") {
            if (analysisState.pendingRangeStart === null) {
              analysisState.pendingRangeStart = xDisplay;
            } else {
              const x0 = Math.min(analysisState.pendingRangeStart, xDisplay);
              const x1 = Math.max(analysisState.pendingRangeStart, xDisplay);
              analysisState.baselineRanges.push({ x0, x1 });
              analysisState.pendingRangeStart = null;
            }
          } else if (analysisState.clickMode === "gaussian") {
            analysisState.gaussianSeeds.push({ xDisplay, y: yVal, freqHz });
          }
          updateAnalysisUI();
          updateOverlays();
        });

      // Tooltip on brush overlay
      brushG.select("rect.overlay")
        .on("mousemove", function (event) {
          const [mouseX, mouseY] = d3.pointer(event);
          const xValue = x.invert(mouseX).toFixed(2);
          const yValue = y.invert(mouseY).toFixed(2);
          tooltip.text(`X: ${xValue} ${xUnit()}, Y: ${yValue}`);
        })
        .on("mouseout", function () {
          tooltip.text("");
        });

      // Double-click to reset zoom
      brushG.on("dblclick", function (event) {
        event.stopPropagation();
        redraw(getDisplayData(), fullXDomain, fullYDomain);
      });

      // Keep tooltip text on top
      tooltip.raise();

      // Store refs for analysis functions
      chartRefs = {
        x, y, brushG, clickOverlay,
        baselineDotsG, baselineCurveG, gaussianDotsG, gaussianCurvesG,
        freqToDisplay, xUnit,
        refreshLine, rescaleAndRedraw,
      };

      // Insert SVG before the button row so buttons appear below the chart
      const btn = document.getElementById("observation-chart-buttons") || document.getElementById("observation-axis-toggle");
      if (btn) {
        container.insertBefore(svg.node(), btn);
      } else {
        container.appendChild(svg.node());
      }

      // Show analysis panel and reset its UI
      const analysisPanel = document.getElementById("analysis-panel");
      if (analysisPanel) analysisPanel.style.display = "";
      document.getElementById("gaussian-results").innerHTML = "";
      updateAnalysisUI();

      // x-axis toggle button
      const axisBtn = document.getElementById("observation-axis-toggle");
      if (axisBtn) {
        if (vlsrCorrection !== null && vlsrCorrection !== undefined) {
          axisBtn.style.display = "";
          axisBtn.textContent = showVlsr ? "Show frequency" : "Show VLSR";
          axisBtn.onclick = function () {
            showVlsr = !showVlsr;
            axisBtn.textContent = showVlsr ? "Show frequency" : "Show VLSR";
            const newData = getDisplayData();
            const newXExtent = d3.extent(newData, (d) => d.x);
            const newYExtent = d3.extent(newData, (d) => d.y);
            const newPad = (newYExtent[1] - newYExtent[0]) * 0.05;
            fullXDomain = newXExtent.slice();
            fullYDomain = [newYExtent[0] - newPad, newYExtent[1] + newPad];
            xLabel.text(showVlsr ? "VLSR (km/s)" : "Frequency (MHz)");
            redraw(newData, fullXDomain, fullYDomain);
            updateCsvLink();
          };
        } else {
          axisBtn.style.display = "none";
        }
      }

      // y-scale toggle button
      const yscaleBtn = document.getElementById("observation-yscale-toggle");
      if (yscaleBtn) {
        yscaleBtn.textContent = showLog ? "Linear scale" : "Log scale";
        yscaleBtn.onclick = function () {
          showLog = !showLog;
          yscaleBtn.textContent = showLog ? "Linear scale" : "Log scale";
          rescaleAndRedraw();
        };
      }
    });
}
