"use strict";
(() => {
  const { copy, riskContent, product, pageMetadata } = window.DDC_CONTENT;
  const config = JSON.parse(document.getElementById('site-config').textContent);
  const params = () => new URLSearchParams(window.location.search);
  let language = preferredLanguage();
  let os = preferredOS();
  let risk = 'safe';
  let revision = 0;
  let toastTimer;
  function preferredLanguage() {
    const explicit = params().get('lang');
    if (explicit === 'en' || explicit === 'zh') return explicit;
    if (window.location.pathname.startsWith('/zh/')) return 'zh';
    try { const saved = localStorage.getItem('ddc-language'); if (saved === 'en' || saved === 'zh') return saved; } catch {}
    return config.initialLanguage;
  }
  function preferredOS() {
    const explicit = params().get('os');
    if (Object.hasOwn(product.cli, explicit)) return explicit;
    try { const saved = localStorage.getItem('ddc-os'); if (Object.hasOwn(product.cli, saved)) return saved; } catch {}
    return 'macos';
  }
  function destination(name, anchor) {
    const url = new URL(config.destinations[language][name], window.location.origin);
    url.searchParams.set('lang', language);
    url.searchParams.set('os', os);
    if (anchor) url.hash = anchor;
    return url.href;
  }
  function refreshLinks() {
    document.querySelectorAll('a[data-destination]').forEach(a => { a.href = destination(a.dataset.destination, a.dataset.anchor); });
    document.querySelectorAll('a[data-reference]').forEach(a => { a.href = language === 'zh' ? '/llms.zh.txt' : '/llms.txt'; });
  }
  function renderAvailability() {
    const release = product.cli[os];
    document.querySelectorAll('[data-os]').forEach(button => { button.disabled = false; button.setAttribute('aria-pressed',String(button.dataset.os === os)); });
    document.querySelectorAll('[data-platform-label]').forEach(el => { el.textContent = release.label; });
    document.querySelectorAll('[data-macos-only]').forEach(el => { el.hidden = !release.available; });
    document.querySelectorAll('[data-unavailable]').forEach(el => { el.hidden = release.available; });
    document.querySelectorAll('[data-copy]').forEach(button => { button.disabled = !!button.closest('[data-platform-command]') && !release.available; });
  }
  function renderRisk() {
    const item = riskContent[language][risk];
    document.querySelectorAll('[data-risk]').forEach(button => button.setAttribute('aria-pressed', String(button.dataset.risk === risk)));
    const id = document.getElementById('risk-id');
    if (!id) return;
    id.textContent = risk;
    for (const field of ['title','description','example','default']) document.getElementById('risk-'+field).textContent = item[field];
  }
  function render(updateUrl=false) {
    revision++;
    const t = copy[language];
    document.documentElement.lang = language === 'zh' ? 'zh-CN' : 'en';
    document.querySelectorAll('[data-i18n]').forEach(el => { if (Object.hasOwn(t,el.dataset.i18n)) el.textContent = t[el.dataset.i18n]; });
    document.querySelectorAll('[data-i18n-aria]').forEach(el => { if (Object.hasOwn(t,el.dataset.i18nAria)) el.setAttribute('aria-label',t[el.dataset.i18nAria]); });
    document.querySelectorAll('[data-i18n-alt]').forEach(el => { if (Object.hasOwn(t,el.dataset.i18nAlt)) el.alt=t[el.dataset.i18nAlt]; });
    document.querySelectorAll('[data-lang]').forEach(button => button.setAttribute('aria-pressed',String(button.dataset.lang === language)));
    const hero=document.getElementById('product-hero-image');
    if(hero) hero.src='/assets/product-hero'+(language==='zh'?'-zh':'')+'.png';
    const meta=pageMetadata[config.page][language];
    document.title=meta.title;
    document.querySelector('meta[name="description"]').content=meta.description;
    for(const [property,value] of Object.entries({
      'og:title':meta.title, 'og:description':meta.description,
      'og:url':config.canonicals[language][config.page], 'og:site_name':product.name,
      'og:locale':language==='zh'?'zh_CN':'en_US',
      'og:locale:alternate':language==='zh'?'en_US':'zh_CN',
      'og:image':meta.imageUrl, 'og:image:secure_url':meta.imageUrl,
      'og:image:type':'image/png', 'og:image:width':String(meta.imageWidth),
      'og:image:height':String(meta.imageHeight), 'og:image:alt':meta.imageAlt
    })) document.querySelector('meta[property="'+property+'"]').content=value;
    for(const [name,value] of Object.entries({
      'twitter:card':'summary_large_image', 'twitter:title':meta.title,
      'twitter:description':meta.description, 'twitter:image':meta.imageUrl,
      'twitter:image:alt':meta.imageAlt
    })) document.querySelector('meta[name="'+name+'"]').content=value;
    document.querySelector('link[rel="canonical"]').href=config.canonicals[language][config.page];
    renderAvailability(); renderRisk(); refreshLinks();
    clearTimeout(toastTimer);
    const status=document.getElementById('copy-status'); status.textContent=''; status.classList.remove('visible');
    if(updateUrl) {
      const url=new URL(window.location.href);
      url.pathname='/'+config.pagePaths[language][config.page];
      url.searchParams.set('lang',language);url.searchParams.set('os',os);
      try { window.history.replaceState(null,'',url.href); } catch {}
      try { localStorage.setItem('ddc-language',language);localStorage.setItem('ddc-os',os); } catch {}
    }
  }
  function announce(message) {
    const status=document.getElementById('copy-status');clearTimeout(toastTimer);status.textContent=message;status.classList.add('visible');
    toastTimer=setTimeout(()=>status.classList.remove('visible'),6500);
  }
  async function copyText(id) {
    const target=document.getElementById(id);
    if(!target || target.closest('[hidden]')) return;
    const guarded=!!target.closest('[data-platform-command]');
    if(guarded&&!product.cli[os].available) {announce(copy[language].osCopyUnavailable);return;}
    const text=target.textContent, requestedLanguage=language,requestedOS=os,request=++revision;
    const current=()=>revision===request&&language===requestedLanguage&&os===requestedOS&&document.getElementById(id)===target&&target.textContent===text&&!target.closest('[hidden]')&&(!guarded||product.cli[os].available);
    try {
      if(!navigator.clipboard?.writeText) throw new Error('Clipboard unavailable');
      await navigator.clipboard.writeText(text);
      if(current()) announce(copy[language].copied);
    } catch {
      if(!current()) return;
      target.focus();
      const selection=window.getSelection();
      if(selection) {const range=document.createRange();range.selectNodeContents(target);selection.removeAllRanges();selection.addRange(range);}
      announce(copy[language].copyFailed);
    }
  }
  document.querySelectorAll('[data-lang]').forEach(button=>button.addEventListener('click',()=>{language=button.dataset.lang;render(true);}));
  document.querySelectorAll('[data-os]').forEach(button=>button.addEventListener('click',()=>{os=button.dataset.os;render(true);}));
  document.querySelectorAll('[data-risk]').forEach(button=>button.addEventListener('click',()=>{risk=button.dataset.risk;renderRisk();}));
  document.querySelectorAll('[data-copy]').forEach(button=>button.addEventListener('click',()=>copyText(button.dataset.copy)));
  window.addEventListener('popstate',()=>{language=preferredLanguage();os=preferredOS();render();});
  render();
})();
