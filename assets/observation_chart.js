function loadObservation(id) {
  const C = 299792458; // m/s
  const F_REST = 1420.405751e6; // Hz

  fetch(`/observations/${id}`)
    .then((res) => {
      if (!res.ok) throw new Error("Failed to fetch observation");
      return res.json();
    })
    .then((data) => {
      const container = document.getElementById("observation-chart");
      container.style.display = "block";

      // Remove any previous SVG
      const existing = container.querySelector("svg");
      if (existing) existing.remove();

      const width = 800;
      const height = 600;
      const margin = 50;

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

      const x = d3.scaleLinear().domain(xExtent).range([margin, width - margin]);
      const y = d3
        .scaleLinear()
        .domain([yExtent[0] - yPadding, yExtent[1] + yPadding])
        .nice()
        .range([height - margin, margin]);

      const svg = d3
        .create("svg")
        .attr("width", width)
        .attr("height", height)
        .attr("viewBox", [0, 0, width, height])
        .attr("style", "max-width: 100%; height: auto;");

      // x-axis
      const xAxisG = svg
        .append("g")
        .attr("transform", `translate(0,${height - margin})`)
        .call(d3.axisBottom(x).ticks(width / 160).tickSizeOuter(0));

      // x-axis label
      const xLabel = svg
        .append("text")
        .attr("x", width / 2)
        .attr("y", height - 10)
        .attr("text-anchor", "middle")
        .attr("font-size", "13px")
        .text(showVlsr ? "VLSR (km/s)" : "Frequency (MHz)");

      // y-axis
      const yAxisG = svg
        .append("g")
        .attr("transform", `translate(${margin},0)`)
        .call(d3.axisLeft(y).ticks(height / 80))
        .call((g) =>
          g
            .append("text")
            .attr("x", -margin)
            .attr("y", 10)
            .attr("text-anchor", "start")
            .text("Amplitude"),
        );

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

      svg
        .append("rect")
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

      container.appendChild(svg.node());

      // Toggle button
      const btn = document.getElementById("observation-axis-toggle");
      if (btn) {
        if (vlsrCorrection !== null && vlsrCorrection !== undefined) {
          btn.style.display = "";
          btn.textContent = showVlsr ? "Show frequency" : "Show VLSR";
          btn.onclick = function () {
            showVlsr = !showVlsr;
            btn.textContent = showVlsr ? "Show frequency" : "Show VLSR";
            const newData = getDisplayData();
            const newXExtent = d3.extent(newData, (d) => d.x);
            x.domain(newXExtent);
            xAxisG.call(d3.axisBottom(x).ticks(width / 160).tickSizeOuter(0));
            xLabel.text(showVlsr ? "VLSR (km/s)" : "Frequency (MHz)");
            path.datum(newData).attr("d", lineFn);
          };
        } else {
          btn.style.display = "none";
        }
      }
    });
}
