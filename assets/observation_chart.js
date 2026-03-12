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

      // Wire up download buttons
      const downloads = document.getElementById("observation-downloads");
      if (downloads) downloads.classList.remove("hidden");
      const dlCsv = document.getElementById("download-csv");
      if (dlCsv) dlCsv.href = `/observations/${id}/csv`;
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

      const startTime = new Date(data.start_time).toUTCString();
      const intTime = Math.round(data.integration_time_secs);
      const titleText = `${data.telescope_id} — ${data.coordinate_system} (${data.target_x.toFixed(1)}, ${data.target_y.toFixed(1)}) — ${intTime}s — ${startTime}`;

      const vlsrCorrection = data.vlsr_correction_mps;
      let showVlsr = vlsrCorrection !== null && vlsrCorrection !== undefined;

      // Store raw frequency data (Hz)
      const rawData = data.frequencies.map((f, i) => ({
        freqHz: f,
        y: data.amplitudes[i],
      }));

      function freqToVlsr(freqHz) {
        return (C * (F_REST - freqHz) / F_REST + vlsrCorrection) / 1000;
      }

      function getDisplayData() {
        if (showVlsr) {
          return rawData.map((d) => ({
            x: freqToVlsr(d.freqHz),
            y: d.y,
          }));
        }
        return rawData.map((d) => ({
          x: d.freqHz / 1e6,
          y: d.y,
        }));
      }

      const points = getDisplayData();

      const xExtent = d3.extent(points, (d) => d.x);
      const yExtent = d3.extent(points, (d) => d.y);
      const yPadding = (yExtent[1] - yExtent[0]) * 0.05;

      let fullXDomain = xExtent.slice();
      let fullYDomain = [yExtent[0] - yPadding, yExtent[1] + yPadding];

      const x = d3.scaleLinear().domain(fullXDomain).range([margin, width - margin]);
      const y = d3
        .scaleLinear()
        .domain(fullYDomain)
        .nice()
        .range([height - margin, margin]);

      const svg = d3
        .create("svg")
        .attr("width", width)
        .attr("height", height)
        .attr("viewBox", [0, 0, width, height])
        .attr("style", "max-width: 100%; height: auto;");

      // Chart title (also included when exporting as PNG)
      svg
        .append("text")
        .attr("x", width / 2)
        .attr("y", 18)
        .attr("text-anchor", "middle")
        .attr("font-size", "12px")
        .attr("fill", "#6b7280")
        .text(titleText);

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
        .call(d3.axisLeft(y).ticks(height / 80))
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

      const path = svg
        .append("path")
        .datum(points)
        .attr("fill", "none")
        .attr("stroke", "steelblue")
        .attr("stroke-width", 1.5)
        .attr("clip-path", "url(#plot-clip)")
        .attr("d", lineFn);

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

      function redraw(currentData, xDom, yDom) {
        x.domain(xDom);
        y.domain(yDom).nice();
        xAxisG.call(d3.axisBottom(x).ticks(width / 160).tickSizeOuter(0))
          .call((g) => g.selectAll("text").attr("font-size", "13px"));
        yAxisG.call(d3.axisLeft(y).ticks(height / 80))
          .call((g) => g.selectAll("text").attr("font-size", "13px"));
        path.datum(currentData).attr("d", lineFn);
      }

      // Brush for zoom
      const brush = d3.brush()
        .extent([[margin, margin], [width - margin, height - margin]])
        .on("end", function (event) {
          if (!event.selection) return;
          const [[px0, py0], [px1, py1]] = event.selection;
          const x0 = x.invert(px0), x1 = x.invert(px1);
          const y0 = y.invert(py1), y1 = y.invert(py0); // py inverted: top is max
          const currentData = getDisplayData();
          redraw(currentData, [x0, x1], [y0, y1]);
          brushG.call(brush.move, null);
        });

      const brushG = svg.append("g")
        .attr("class", "brush")
        .call(brush);

      // Tooltip on brush overlay
      brushG.select("rect.overlay")
        .on("mousemove", function (event) {
          const [mouseX, mouseY] = d3.pointer(event);
          const xValue = x.invert(mouseX).toFixed(2);
          const yValue = y.invert(mouseY).toFixed(2);
          const xUnit = showVlsr ? "km/s" : "MHz";
          tooltip.text(`X: ${xValue} ${xUnit}, Y: ${yValue}`);
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

      // Insert SVG before the toggle button so the button appears below the chart
      const btn = document.getElementById("observation-axis-toggle");
      if (btn) {
        container.insertBefore(svg.node(), btn);
      } else {
        container.appendChild(svg.node());
      }

      // Toggle button
      if (btn) {
        if (vlsrCorrection !== null && vlsrCorrection !== undefined) {
          btn.style.display = "";
          btn.textContent = showVlsr ? "Show frequency" : "Show VLSR";
          btn.onclick = function () {
            showVlsr = !showVlsr;
            btn.textContent = showVlsr ? "Show frequency" : "Show VLSR";
            const newData = getDisplayData();
            const newXExtent = d3.extent(newData, (d) => d.x);
            const newYExtent = d3.extent(newData, (d) => d.y);
            const newPad = (newYExtent[1] - newYExtent[0]) * 0.05;
            fullXDomain = newXExtent.slice();
            fullYDomain = [newYExtent[0] - newPad, newYExtent[1] + newPad];
            xLabel.text(showVlsr ? "VLSR (km/s)" : "Frequency (MHz)");
            redraw(newData, fullXDomain, fullYDomain);
          };
        } else {
          btn.style.display = "none";
        }
      }
    });
}
