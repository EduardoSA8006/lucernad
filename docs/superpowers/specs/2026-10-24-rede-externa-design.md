# lucernad: o módulo de rede externa (letras e clima)

**Data:** 2026-10-24
**Base:** os specs anteriores do `lucernad` (o daemon, o `lucernad-core`, a conexão permanente do shell, o cliente com donos e filtro pelo `lucerna.hello`, "disponível" = conectado e sem erro, testes do shell pelo caminho real do sinal e da resposta).
**Repositórios:** `lucernad` (o módulo `web`) e `lucerna-shell` (os serviços `Lyrics.qml` e `Weather.qml` viram adaptadores; a aba Clima é redesenhada).

## Por quê

O shell busca a letra (LRCLIB) e o clima (Open-Meteo) direto pelo `XMLHttpRequest` do QML. Sem cache em disco, tudo é buscado de novo a cada reinício; uma falha de rede vira "Sem letra" até reiniciar; não há prazo (a busca pode ficar "Procurando a letra…" para sempre, e há um bug que deixa o `loading` ligado ao voltar a uma faixa já buscada); a busca reserva da letra pega o primeiro resultado sem conferir a duração e pode mostrar a letra da faixa errada; o LRC com várias marcas de tempo por linha sai incompleto; sem internet o clima tenta, falha e mostra erro. A lógica vai para código próprio em Rust e a aba Clima ganha o desenho minimalista do Caelestia.

## Decisões do usuário

- Pulou as perguntas e aprovou as recomendações do construtor do documento (2026-10-24), abaixo.
- **Segunda fonte de letras: só o Vagalume**, consultado quando a LRCLIB não acha (música brasileira; só texto, sem sincronização). Descartados: NetEase, Musixmatch, Spotify, Genius, Letras.mus.br, Lyrics.ovh (termos ou estabilidade).
- **Aba Clima copiada do Caelestia** (`caelestia-dots/shell`, `modules/dashboard/WeatherTab.qml`), adaptada à fonte, aos tamanhos de texto, aos ícones e ao tema do Lucerna, com o minimalismo dele. **A previsão hora a hora sai.**
- **Sem reserva**, como nos outros módulos: sem o `lucernad`, a letra e o clima ficam indisponíveis.
- **Créditos num lugar só** (2026-10-24): uma página "Créditos" nas Configurações e a seção "Créditos" do README; nenhum crédito nos painéis (sai o "Previsão da Open-Meteo" da aba Clima, a letra não ganha rótulo do Vagalume) nem na tela Sobre (sai a linha "Serviços na internet").
- **Vagalume é escolha de quem usa** (2026-10-24): os termos da API pedem crédito com link junto do conteúdo e dizem que a API não transfere o direito de exibir a letra. Como o shell é de uso próprio e a chave é criada por cada usuário, o README avisa disso e deixa a decisão com ele; se o Vagalume pedir, o rótulo com link junto da letra é acrescentado depois.

## Objetivos

- Letras com prazo, nova tentativa em erro passageiro, casamento confiável e cache em disco.
- Vagalume como segunda fonte, com a chave do próprio usuário (o aviso sobre os termos fica no README).
- Clima que abre na hora com a última previsão guardada, se atualiza sozinho no fundo e respeita a falta de internet.
- Escolha da cidade entre vários resultados.
- A aba Clima nova: cidade e data; nascer e pôr do sol; card grande com ícone, temperatura e descrição; umidade, sensação e vento; previsão de 7 dias.

## Não objetivos

- Outras fontes de letra além da LRCLIB e do Vagalume.
- Letra por palavra (karaokê) ou tradução.
- Localização automática (por IP ou GPS).
- Alertas de tempo, mapas, qualidade do ar.

## Anti-objetivos

- Mostrar a letra de outra música (na dúvida, "Sem letra").
- Enviar mais do que hoje: só título, artista, álbum e duração da faixa às fontes de letra, e o nome da cidade / as coordenadas ao Open-Meteo; nunca localização por IP.
- Tentar a rede com o modo offline ligado.
- Embutir uma chave do Vagalume no código.

## O módulo `web`

### Cliente HTTP

- Código próprio em Rust (ex.: `reqwest` com rustls ou `ureq`; escolha do construtor), com prazo por pedido (~10 s), `User-Agent` do Lucerna com o link do repositório (a LRCLIB pede), e nova tentativa com espera crescente em erro passageiro (timeout, 5xx, 429 respeitando `Retry-After`), no máximo 3.
- **Modo offline** (`web.configure {offline}`): nenhum pedido sai.
- **Sem conexão:** o módulo acompanha a conectividade do NetworkManager (D-Bus, `Connectivity`/`StateChanged`) e não tenta a rede quando ela é `none`; sem NetworkManager, tenta normalmente. O clima e a letra mostram o que está guardado.

### Letras

- **Ordem:** LRCLIB `get` (título, artista, álbum, duração) → LRCLIB `search` (título e artista) → Vagalume (só com a chave configurada).
- **Casamento confiável na busca reserva e no Vagalume:** só aceita um resultado com título e artista iguais depois de normalizados (sem acentos, maiúsculas, pontuação, sufixos como "(Remastered)"/"- Ao Vivo") e, quando a fonte informa a duração, diferença de até 2 s. Prefere letra sincronizada. Na dúvida, nenhuma letra.
- **LRC completo:** várias marcas de tempo na mesma linha, o `[offset:]`, linhas vazias como pausa; o resultado é uma lista ordenada de `{time, text}`.
- **Vagalume:** `api.vagalume.com.br/search.php` com `apikey` do usuário (configurada no shell e mandada pelo `web.configure`); letra só texto; o resultado carrega a origem (`source: "vagalume"`, `url` da página da letra), que o shell não mostra hoje mas deixa pronto o rótulo com link se o Vagalume pedir.
- **Cache em disco** (em `$XDG_CACHE_HOME/lucerna/web/`, pela `lucerna-paths`): a letra encontrada fica guardada sem prazo; "sem letra" fica guardado por 7 dias (depois tenta de novo); falha de rede nunca é guardada. Limite de tamanho do cache (ex.: 50 MB, apaga as mais antigas). Chave: artista + título + duração arredondada, normalizados.
- **Quando busca:** o shell manda `web.track` a cada troca de faixa (é informação local, não sai da máquina), mas o módulo só consulta as fontes **com assinante de `web.lyrics`** — o shell assina só com a aba Mídia visível e a letra ligada, como hoje; ao ganhar assinante, busca a faixa atual. Assim nada vai à internet além do que vai hoje (decisão do construtor do documento).
- **Sem bug de carregando:** pedir a letra de uma faixa já em cache responde na hora e nunca deixa o estado em "procurando".

### Clima

- **Previsão** do Open-Meteo como hoje, com `daily` acrescido de `sunrise,sunset` e `forecast_days=7`; sem `hourly` (a faixa por hora sai da interface).
- **Cache em disco:** a última previsão de cada local fica guardada com o horário; ao assinar, o módulo publica a guardada na hora (com `updatedAt`) e, se estiver mais velha que o intervalo configurado (`refresh`, 30 min por padrão), busca de novo no fundo. Só busca com assinante (a aba ou o card visíveis, como hoje).
- **Busca de cidade:** `web.searchCity {query}` → até 5 resultados do geocoding (`count=5`, `language=pt`), com nome, estado, país e coordenadas; o shell mostra a lista e guarda a escolhida na configuração, como hoje.

### Tópicos e métodos

- `web.lyrics` — `{key, state: "searching"|"found"|"none"|"error", synced, lines: [{time, text}], source, url?, error?}` da faixa atual.
- `web.weather` — `{location, updatedAt, current: {temperature, apparent, humidity, wind, code, isDay}, daily: [{date, code, min, max, precipitation}], sunrise, sunset, state, error?}` (até 7 dias; unidades em °C e km/h, a conversão continua no shell).
- `web.status` — `{available, error, online}`.
- `web.track {artist, title, album?, duration?}` → `{}`: a faixa tocando agora (o shell manda ao mudar).
- `web.configure {offline, vagalumeKey?, location?, refresh?}` → `{}`.
- `web.searchCity {query}` → `{results: [...]}`; `web.refresh {}` → `{}` (o botão de atualizar).
- Erros de parâmetro com o `-32602` do SDK e texto em pt-BR; a chave do Vagalume nunca aparece em log nem em tópico.

## Lado do shell (`lucerna-shell`)

- **`services/Lyrics.qml` e `services/Weather.qml` viram adaptadores** (donos `"lyrics"` e `"weather"`), mantendo para quem usa o que a aba Mídia, o card do Painel e a aba Clima precisam; o `XMLHttpRequest` sai.
- **Letra:** quando a fonte é o Vagalume, a letra aparece como texto rolável (sem acompanhar a música) sem rótulo da fonte (os créditos ficam na página Créditos).
- **Configurações:** um campo para a chave do Vagalume (com o link de onde criar), guardado como os outros segredos do shell (ou na configuração, se não houver lugar de segredo; escolha do construtor), e a busca de cidade com a lista de resultados.
- **Aba Clima nova**, no desenho do Caelestia (`WeatherTab.qml`), com a fonte, os tamanhos, os ícones e o tema do Lucerna:
  - topo: a cidade em destaque e a data por extenso; à direita, nascer e pôr do sol com ícones;
  - card grande: ícone do tempo, temperatura grande e a descrição em português;
  - três cards: umidade, sensação térmica e vento (ícone, rótulo, valor);
  - "Previsão de 7 dias": um card por dia com "Hoje"/dia da semana abreviado, a data, o ícone e mínima/máxima;
  - quando a previsão for a guardada e velha, a hora da última atualização; sem internet ou offline, o aviso de hoje.
  - **Sem campos de busca nem botões na aba** (o minimalismo do Caelestia): a escolha da cidade, com a lista de resultados, fica só em Configurações; sem cidade escolhida, a aba mostra um aviso com um botão que abre essa página. A atualização é automática; "Atualizar agora" fica nas Configurações (e no IPC), pelo `web.refresh` (decisão do construtor do documento).
- **Créditos:** página nova "Créditos" nas Configurações, com link em cada item: inspiração (Caelestia Shell: painel superior, aba Clima, curvas de animação do Material 3), dados (Open-Meteo com a licença CC BY 4.0, LRCLIB, Vagalume) e base (Quickshell, Hyprland, fonte Material Symbols). O README mantém a seção "Créditos" com o mesmo conteúdo e ganha o aviso sobre os termos do Vagalume. Saem o rodapé "Previsão da Open-Meteo" da aba Clima e a linha "Serviços na internet" da tela Sobre.
- **Sem o `lucernad`:** "Indisponível" na aba Clima, no card e na letra.

## Testes

- **Rust:** o parse do LRC (várias marcas, offset, linhas vazias); a normalização e o casamento (título/artista/duração, sufixos, acentos; resultado errado recusado); a ordem das fontes e o Vagalume só com chave; o cache (letra sem prazo, "sem letra" por 7 dias, falha de rede não guardada, limite de tamanho); o clima guardado publicado na hora e a busca no fundo quando velho; offline e sem conexão sem nenhum pedido; nova tentativa e `Retry-After`. Rede falsa (servidor HTTP local de teste ou dublê do cliente) — nenhum teste automatizado sai para a internet.
- **Máquina real:** algumas faixas no Spotify (inclusive uma brasileira que só o Vagalume tenha), o clima abrindo na hora depois de reiniciar, a busca de cidade com vários resultados, o modo offline.
- **Shell:** os adaptadores pelo caminho real, a aba Clima nova conferida visualmente no container, fumaça sem avisos, cobertura 100%.

## O que fica para o construtor

- As bibliotecas (HTTP, TLS, D-Bus do NetworkManager) e o formato do cache.
- A lista exata de sufixos ignorados no casamento e o limite do cache.
- Onde a chave do Vagalume fica guardada no shell.
- Os ícones de tempo do tema do Lucerna para cada código WMO.
