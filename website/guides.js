// Copy buttons on example prompts, and the masthead's scrolled state.
for (const button of document.querySelectorAll('.prompt-copy')) {
  button.addEventListener('click', async () => {
    const text = button.parentElement.querySelector('p').textContent;
    try { await navigator.clipboard.writeText(text); button.textContent = 'Copied'; }
    catch { button.textContent = 'Select to copy'; }
    setTimeout(() => { button.textContent = 'Copy'; }, 1600);
  });
}
const masthead = document.querySelector('.site-header');
if (masthead) {
  const sync = () => masthead.classList.toggle('scrolled', scrollY > 8);
  addEventListener('scroll', sync, { passive: true });
  sync();
}
