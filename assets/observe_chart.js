function get_telescope_from_location() {
  const path_parts = window.location.pathname.split('/')
  // Loop to one less to allow picking an item one ahead of i.
  for (let i = 0; i < path_parts.length - 1; i++) {
    if (path_parts[i] == "observe") return path_parts[i + 1];
  }
  throw Error("Failed to find a telescope from the URL path");
}

(function () {
  const C = 299792458; // m/s
  const F_REST = 1420.405751e6; // Hz

  const width = 640;
  const height = 480;
  const margin = 50;

  let vlsrCorrection = null;
  let showVlsr = false;
  let showLog = false;
  let latestData = null; // store raw frequency (Hz) data

  function freqToVlsr(freqHz) {
    return (C * (F_REST - freqHz) / F_REST + vlsrCorrection) / 1000;
  }

  const x = d3
    .scaleLinear()
    .domain([0, 100]) // These values will be replaced.
    .range([margin, width - margin]);
  let y = d3
    .scaleLinear()
    .domain([0, 20]) // These values are completely arbitrary.
    .range([height - margin, margin]);
  const svg = d3
    .create("svg")
    .attr("id", "measurement")
    .attr("width", width)
    .attr("height", height)
    .attr("viewBox", [0, 0, width, height])
    .attr("style", "max-width: 100%; height: auto; height: intrinsic;");
  const line = d3
    .line()
    .x((d) => x(d.x))
    .y((d) => y(d.y));
  // x-axis
  const xAxis = svg
    .append("g")
    .attr("transform", `translate(0,${height - margin})`)
    .call(
      d3
        .axisBottom(x)
        .ticks(width / 160)
        .tickSizeOuter(0),
    );
  // x-axis label
  const xLabel = svg
    .append("text")
    .attr("x", width / 2)
    .attr("y", height - 10)
    .attr("text-anchor", "middle")
    .attr("font-size", "13px")
    .text("Frequency (MHz)");
  // chart title (integration time)
  const chartTitle = svg
    .append("text")
    .attr("x", width / 2)
    .attr("y", margin / 2)
    .attr("text-anchor", "middle")
    .attr("font-size", "12px")
    .attr("fill", "#6b7280");
  // y-axis
  const yTickFormat = d3.format(".3~s");
  const yAxis = svg
    .append("g")
    .attr("transform", `translate(${margin},0)`)
    .call(d3.axisLeft(y).ticks(height / 80).tickFormat(yTickFormat));

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
  svg
    .append("path")
    .attr("class", "line")
    .attr("fill", "none")
    .attr("stroke", "steelblue")
    .attr("stroke-width", 1.5);
  // Insert SVG before the toggle button so the button appears below the chart
  const chartContainer = document.currentScript.parentElement;
  const toggleBtn = document.getElementById("observe-axis-toggle");
  if (toggleBtn) {
    chartContainer.insertBefore(svg.node(), toggleBtn);
  } else {
    chartContainer.appendChild(svg.node());
  }
  // Tooltip for displaying coordinates
  const tooltip = svg.append("text")
        .attr("id", "tooltip")
        .attr("x", width - margin)
        .attr("y", margin)
        .attr("text-anchor", "end")
        .attr("alignment-baseline", "hanging")
        .attr("font-size", "12px")
        .attr("fill", "black");

  // Transparent rect for capturing mouse movement
  svg.append("rect")
    .attr("width", width - 2 * margin)
    .attr("height", height - 2 * margin)
    .attr("x", margin)
    .attr("y", margin)
    .style("fill", "none")
    .style("pointer-events", "all")
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

  function getDisplayData(rawData) {
    if (showVlsr && vlsrCorrection !== null) {
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

  function updateChart() {
    if (!latestData) return;
    const data = getDisplayData(latestData);
    if (showLog) {
      const yMax = d3.max(data, (d) => d.y);
      const yFloor = Math.max(yMax * 1e-4, 1e-6);
      const clipped = data.map((d) => ({ x: d.x, y: Math.max(d.y, yFloor) }));
      y = d3.scaleLog().domain([yFloor, yMax]).range([height - margin, margin]).clamp(true);
      yAxis.call(d3.axisLeft(y).ticks(6, ".2~e"));
      svg.select(".line").datum(clipped).attr("d", d3.line().x((d) => x(d.x)).y((d) => y(d.y)));
    } else {
      const yRange = d3.extent(data, (d) => d.y);
      const padding = (yRange[1] - yRange[0]) * 0.05;
      y = d3.scaleLinear().domain([yRange[0] - padding, yRange[1] + padding]).range([height - margin, margin]).nice();
      yAxis.call(d3.axisLeft(y).ticks(height / 80).tickFormat(yTickFormat));
      svg.select(".line").datum(data).attr("d", d3.line().x((d) => x(d.x)).y((d) => y(d.y)));
    }
    const xRange = d3.extent(data, (d) => d.x);
    x.domain(xRange);
    xAxis.call(d3.axisBottom(x));
    xLabel.text(showVlsr ? "VLSR (km/s)" : "Frequency (MHz)");

    // Update power level: average of center 50% of spectrum
    const n = latestData.length;
    const lo = Math.floor(n * 0.25);
    const hi = Math.ceil(n * 0.75);
    const center = latestData.slice(lo, hi);
    window.observePowerLevel = center.reduce((s, d) => s + d.y, 0) / center.length;
  }

  window.setChartTitle = function(text) {
    chartTitle.text(text);
  };

  // Expose toggle functions for the buttons
  window.toggleObserveYScale = function () {
    showLog = !showLog;
    const btn = document.getElementById("observe-yscale-toggle");
    if (btn) btn.textContent = showLog ? "Linear scale" : "Log scale";
    updateChart();
  };

  window.setObserveLogScale = function (log) {
    showLog = log;
    const btn = document.getElementById("observe-yscale-toggle");
    if (btn) btn.textContent = showLog ? "Linear scale" : "Log scale";
    updateChart();
  };

  window.toggleObserveAxis = function () {
    if (vlsrCorrection === null) return;
    showVlsr = !showVlsr;
    const btn = document.getElementById("observe-axis-toggle");
    if (btn) btn.textContent = showVlsr ? "Show frequency" : "Show VLSR";
    updateChart();
  };

  function connectSpectrumSocket() {
    if (window.spectrumSocket) {
      window.spectrumSocket.close();
    }
    let receivedMetadata = false;
    window.spectrumSocket = new WebSocket(`/telescope/${get_telescope_from_location()}/spectrum`);
    window.spectrumSocket.onmessage = async (event) => {
      // First message is JSON text with metadata
      if (!receivedMetadata) {
        receivedMetadata = true;
        const meta = JSON.parse(typeof event.data === "string" ? event.data : await event.data.text());
        vlsrCorrection = meta.vlsr_correction_mps;
        const btn = document.getElementById("observe-axis-toggle");
        if (vlsrCorrection !== null) {
          showVlsr = true;
          if (btn) {
            btn.style.display = "";
            btn.textContent = "Show frequency";
          }
        } else {
          showVlsr = false;
          if (btn) btn.style.display = "none";
        }
        return;
      }

    let dataView = new DataView(await event.data.arrayBuffer());
    latestData = [];
    // The data is interleaved (freq, spectrum).
    for (let i = 0; i < dataView.byteLength; i += 16) {
      latestData.push({
        freqHz: dataView.getFloat64(i, true),
        y: dataView.getFloat64(i + 8, true),
      });
    }
      updateChart();
    };
  }

  window.reconnectSpectrumSocket = connectSpectrumSocket;
  connectSpectrumSocket();
})();
