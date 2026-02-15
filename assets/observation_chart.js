function loadObservation(id) {
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

      const points = data.frequencies.map((f, i) => ({
        x: f / 1e6, // Convert to MHz
        y: data.amplitudes[i],
      }));

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
      svg
        .append("g")
        .attr("transform", `translate(0,${height - margin})`)
        .call(d3.axisBottom(x).ticks(width / 160).tickSizeOuter(0));

      // y-axis
      svg
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
      const line = d3
        .line()
        .x((d) => x(d.x))
        .y((d) => y(d.y));

      svg
        .append("path")
        .datum(points)
        .attr("fill", "none")
        .attr("stroke", "steelblue")
        .attr("stroke-width", 1.5)
        .attr("d", line);

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
          tooltip.text(`X: ${xValue} MHz, Y: ${yValue}`);
        })
        .on("mouseout", function () {
          tooltip.text("");
        });

      container.appendChild(svg.node());
    });
}
