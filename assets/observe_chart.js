function get_telescope_from_location() {
  const path_parts = window.location.pathname.split('/')
  // Loop to one less to allow picking an item one ahead of i.
  for (let i = 0; i < path_parts.length - 1; i++) {
    if (path_parts[i] == "observe") return path_parts[i + 1];
  }
  throw Error("Failed to find a telescope from the URL path");
}

(function () {
  const width = 800;
  const height = 600;
  const margin = 50;

  const x = d3
    .scaleLinear()
    .domain([0, 100]) // These values will be replaced.
    .range([margin, width - margin]);
  const y = d3
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
  xAxis = svg
    .append("g")
    .attr("transform", `translate(0,${height - margin})`)
    .call(
      d3
        .axisBottom(x)
        .ticks(width / 160)
        .tickSizeOuter(0),
    );
  // y-axis
  const yAxis = svg
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
  svg
    .append("path")
    .attr("class", "line")
    .attr("fill", "none")
    .attr("stroke", "steelblue")
    .attr("stroke-width", 1.5);
  document.currentScript.parentElement.appendChild(svg.node());
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
      tooltip.text(`X: ${xValue} MHz, Y: ${yValue}`);
    })
    .on("mouseout", function () {
      tooltip.text("");
    });
  // There can be a socket already here if the page is refetched by htmx.
  if (window.spectrumSocket) {
    window.spectrumSocket.close();
  }
  window.spectrumSocket = new WebSocket(`/telescope/${get_telescope_from_location()}/spectrum`);
  window.spectrumSocket.onmessage = async (event) => {
    let dataView = new DataView(await event.data.arrayBuffer());
    let data = [];
    // The data is interleaved (freq, spectrum).
    for (let i = 0; i < dataView.byteLength; i += 16) {
      data.push({
        // Convert to MHz immediately for display.
        x: dataView.getFloat64(i, true) / 1e6,
        y: dataView.getFloat64(i + 8, true),
      });
    }
    const yRange = d3.extent(data, (d) => d.y);
    const padding = (yRange[1] - yRange[0]) * 0.05;
    y.domain([yRange[0] - padding, yRange[1] + padding]).nice();
    yAxis.call(d3.axisLeft(y).ticks(height / 80));
    const xRange = d3.extent(data, (d) => d.x);
    x.domain(xRange);
    xAxis.call(d3.axisBottom(x));
    const line = d3
      .line()
      .x((d) => x(d.x))
      .y((d) => y(d.y));
    svg.select(".line").datum(data).attr("d", line);
  };
})();
