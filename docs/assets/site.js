document.querySelectorAll('[role="tab"]').forEach((tab, _, tabs) => {
  tab.addEventListener("click", () => {
    for (const t of tabs) {
      t.setAttribute("aria-selected", t === tab);
      document.getElementById(t.getAttribute("aria-controls")).hidden = t !== tab;
    }
  });
});

document.querySelectorAll("[data-copy]").forEach((button) => {
  button.addEventListener("click", async () => {
    await navigator.clipboard.writeText(button.dataset.copy);
    button.textContent = "Copied";
    button.classList.add("copied");
    clearTimeout(button.timer);
    button.timer = setTimeout(() => {
      button.textContent = "Copy";
      button.classList.remove("copied");
    }, 1600);
  });
});
