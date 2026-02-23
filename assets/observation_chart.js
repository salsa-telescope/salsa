function loadObservation(id) {
  const C = 299792458; // m/s
  const F_REST = 1420.405751e6; // Hz

  fetch(`/observations/${id}`)
    .then((res) => {
      if (!res.ok) throw new Error("Failed to fetch observation");
      return res.json();
    })
    .then((data) => {
      // Highlight selected observation row
      document.querySelectorAll("[id^='obs-row-']").forEach((el) => {
        el.classList.remove("bg-indigo-50", "border-indigo-300");
        el.classList.add("border-transparent");
      });
      const selectedRow = document.getElementById(`obs-row-${id}`);
      if (selectedRow) {
        selectedRow.classList.remove("border-transparent");
        selectedRow.classList.add("bg-indigo-50", "border-indigo-300");
      }

      const container = document.getElementById("observation-chart");
      container.style.display = "block";

      // Update title with observation summary
      const title = document.getElementById("observation-title");
      if (title) {
        const startTime = new Date(data.start_time).toUTCString();
        const intTime = Math.round(data.integration_time_secs);
        title.textContent = `${data.telescope_id} — ${data.coordinate_system} (${data.target_x.toFixed(1)}, ${data.target_y.toFixed(1)}) — ${intTime}s — ${startTime}`;
      }

      // Remove any previous SVG
      const existing = container.querySelector("svg");
      if (existing) existing.remove();

      const width = 640;
      const height = 420;
      const margin = 60;

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
            x.domain(newXExtent);
            xAxisG.call(d3.axisBottom(x).ticks(width / 160).tickSizeOuter(0))
              .call((g) => g.selectAll("text").attr("font-size", "13px"));
            xLabel.text(showVlsr ? "VLSR (km/s)" : "Frequency (MHz)");
            path.datum(newData).attr("d", lineFn);
          };
        } else {
          btn.style.display = "none";
        }
      }
    });
}
