const menuButton = document.querySelector(".menu-button");
const navLinks = document.querySelector("#nav-links");

menuButton?.addEventListener("click", () => {
  const open = menuButton.getAttribute("aria-expanded") !== "true";
  menuButton.setAttribute("aria-expanded", String(open));
  navLinks?.classList.toggle("open", open);
});

navLinks?.addEventListener("click", (event) => {
  if (event.target instanceof HTMLAnchorElement) {
    menuButton?.setAttribute("aria-expanded", "false");
    navLinks.classList.remove("open");
  }
});

const commands = {
  cargo:
    "cargo install --git https://github.com/hjosugi/frost-build --tag v0.6.1 frostbuild-cli",
  source:
    "git clone https://github.com/hjosugi/frost-build && cd frost-build && cargo build --release --locked",
};

const commandElement = document.querySelector("#install-command");
const tabs = document.querySelectorAll(".install-tabs button");
const copyButton = document.querySelector(".copy-button");

tabs.forEach((tab) => {
  tab.addEventListener("click", () => {
    tabs.forEach((candidate) => {
      const active = candidate === tab;
      candidate.classList.toggle("active", active);
      candidate.setAttribute("aria-selected", String(active));
    });
    const key = tab.getAttribute("data-command");
    if (commandElement && key && commands[key]) commandElement.textContent = commands[key];
    if (copyButton) copyButton.textContent = "Copy";
  });
});

copyButton?.addEventListener("click", async () => {
  if (!commandElement) return;
  try {
    await navigator.clipboard.writeText(commandElement.textContent || "");
    copyButton.textContent = "Copied";
  } catch {
    copyButton.textContent = "Select text";
  }
});
