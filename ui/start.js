if (new URLSearchParams(location.search).get("lang") === "en") {
  document.documentElement.lang = "en";
  document.title = "NieNudno — New tab";
  document.getElementById("search-label").textContent = "Search the web";
  document.getElementById("query").placeholder = "What are you looking for?";
  document.getElementById("search-button").textContent = "Search";
}
