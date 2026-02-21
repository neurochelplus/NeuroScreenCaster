# **Архитектура метаданных в системах записи экрана: Технический анализ ScreenStudio и CANVID, и руководство по разработке аналога**

## **1\. Введение: Смена парадигмы от растрового захвата к событийно-ориентированному рендерингу**

Традиционные инструменты записи экрана, такие как OBS Studio, QuickTime или Camtasia (в ранних версиях), работали по принципу "плоского" захвата: они записывали пиксельный поток с кадрового буфера видеокарты, "впекая" курсор мыши и элементы интерфейса непосредственно в растровое изображение видеофайла. Этот подход, будучи технически простым, накладывал жесткие ограничения на пост-продакшн. Если пользователь делал резкое движение мышью, видео получалось дерганым. Если разрешение экрана было слишком высоким (4K), мелкие элементы интерфейса становились нечитаемыми на мобильных устройствах без ручного кадрирования.

Новое поколение инструментов, возглавляемое **ScreenStudio** (macOS) и **CANVID** (кроссплатформенное решение), использует принципиально иную архитектуру — **Metadata-First Recording (Запись на основе метаданных)**. В этой модели процесс записи разделяется на два независимых, но синхронизированных потока:

1. **Чистый видеопоток**: Запись фона (окон приложений, рабочего стола) без системного курсора и оверлеев.  
2. **Поток событий (Telemetry Stream)**: Высокочастотное логирование координат мыши, кликов, нажатий клавиш, изменений активного окна и их геометрических границ (bounding boxes).

Финальное видео не "записывается" в привычном смысле, а "рендерится" или "собирается" в реальном времени движком композитинга. Курсор мыши — это не пиксели на видео, а векторный объект (SVG), который движется по математически сглаженной траектории поверх видеофона. Зум — это не просто цифровое увеличение, а программно управляемая виртуальная камера, которая следует за координатами событий.

Этот отчет представляет собой исчерпывающее исследование функциональности, технологического стека и алгоритмической базы таких программ, а также содержит детальное руководство по разработке собственного аналога с использованием современных технологий (Rust, Tauri, React, Remotion).

## ---

**2\. Глубокий анализ функциональности и UX-паттернов**

### **2.1. ScreenStudio: Эталон автоматизации на macOS**

ScreenStudio, разработанная для macOS, стала де\-факто стандартом для создания демонстрационных видео благодаря глубокой интеграции с нативными API Apple. Программа позиционируется как инструмент, который "автоматически делает видео профессиональным", минимизируя участие пользователя в монтаже.1

#### **Ключевые функции и их механизмы**

1. **Автоматический умный зум (Auto-Zoom)**  
   * **Описание**: Камера плавно приближается к месту взаимодействия (клику, вводу текста), автоматически определяя оптимальный уровень масштабирования.  
   * **Механизм**: Программа не просто зумит в координаты курсора ![][image1]. Она использует Accessibility API macOS для определения границ UI-элемента (кнопки, текстового поля), с которым происходит взаимодействие. Это позволяет центрировать "камеру" не на курсоре, а на логическом центре элемента, добавляя эстетически выверенные отступы (padding).3  
   * **Режимы**: Пользователь может переключаться между автоматическим режимом (зум на каждый клик) и ручным (выбор областей на таймлайне).3  
2. **Сглаживание курсора (Cursor Smoothing)**  
   * **Описание**: Дрожащие, неуверенные движения мыши превращаются в плавные дуги.  
   * **Механизм**: Системный курсор скрыт при записи. Во время воспроизведения векторный макет курсора движется по интерполированной кривой (обычно сплайны Безье или Катмулла-Рома), построенной на основе ключевых точек (начало движения, клик, конец движения).1  
   * **Кастомизация**: Возможность изменить размер курсора постфактум, сменить его тип (например, на touch-индикатор для имитации iOS).4  
3. **Неразрушающее редактирование (Non-Destructive Editing)**  
   * **Описание**: Изменение фона, теней окон, скруглений углов и пропорций видео (вертикальное/горизонтальное) возможно *после* записи без потери качества.  
   * **Механизм**: Исходный видеофайл (Raw Footage) остается неизменным. Все эффекты — это параметры в JSON-файле проекта (.screenstudio), которые применяются движком рендеринга в реальном времени.5

### **2.2. CANVID: Кроссплатформенный вызов**

CANVID предлагает схожий набор функций, но работает и на Windows, что накладывает определенные ограничения и требует иных технологических решений.7

#### **Особенности реализации в CANVID**

1. **Гибридный зум (Auto & Manual Zoom)**  
   * **Реализация**: Как и ScreenStudio, CANVID анализирует поток событий кликов. В редакторе зумы отображаются как сегменты на таймлайне, которые можно растягивать, удалять или изменять их интенсивность.7  
   * **AI-Enhancements**: CANVID активно использует ИИ для улучшения качества голоса (удаление шума) и обработки фона веб\-камеры без использования хромакея (зеленого экрана).8  
2. **Технологические отличия**:  
   * В то время как ScreenStudio полагается на нативные API macOS для рендеринга, CANVID, вероятно, использует веб\-технологии (Electron/Chromium) для создания интерфейса редактора и рендеринга превью, что делает его более гибким, но потенциально более требовательным к ресурсам.10

## ---

**3\. Технологический стек: "Под капотом" лидеров рынка**

Чтобы создать аналог, необходимо деконструировать технологии, используемые в существующих решениях. Анализ показывает четкое разделение на "Нативный" (ScreenStudio) и "Гибридный" (CANVID, Remotion-based) подходы.

### **3.1. Стек ScreenStudio (Native macOS)**

ScreenStudio достигает высокой производительности за счет отказа от кроссплатформенных прослоек в критических узлах захвата и обработки.

| Компонент | Технология | Обоснование |
| :---- | :---- | :---- |
| **Язык разработки** | **Swift** | Нативный язык для экосистемы Apple, обеспечивающий прямой доступ к системным фреймворкам.12 |
| **Захват видео** | **ScreenCaptureKit (SCK)** | Введенный в macOS 12.3 фреймворк, позволяющий захватывать видео с GPU с нулевым копированием (zero-copy). SCK позволяет исключать определенные окна (например, окно самого рекордера) и системный курсор из захвата на уровне композитора.13 |
| **Метаданные UI** | **AXUIElement (Accessibility API)** | Используется для получения координат и размеров окон и кнопок под курсором. Это критически важно для "умного" зума, который знает контекст клика.15 |
| **Ввод (Input)** | **CGEventTap / NSEvent** | Глобальный перехват событий мыши и клавиатуры для построения таймлайна действий. |
| **Рендеринг** | **Metal / CoreAnimation** | Использование GPU для композитинга слоев (фон, видео, курсор) в реальном времени. |
| **Кодирование** | **VideoToolbox** | Аппаратное ускорение кодирования H.264/HEVC. |

### **3.2. Стек CANVID и веб\-ориентированных аналогов**

Для работы на Windows и macOS используется стек, построенный вокруг веб\-технологий, обернутых в нативную оболочку.

| Компонент | Технология | Обоснование |
| :---- | :---- | :---- |
| **Язык разработки** | **TypeScript / JavaScript** | Единая кодовая база для UI и логики редактора. |
| **Оболочка** | **Electron** | Обеспечивает среду выполнения для веб\-приложения на десктопе.10 |
| **Захват (Win)** | **Windows.Graphics.Capture (WGC)** | Современный API Windows 10/11 для захвата окон и экранов. Позволяет отключать захват курсора и захватывать конкретные окна даже если они перекрыты.18 |
| **Рендеринг видео** | **Remotion / WebGL** | Использование React для описания видеосцен. Кадры рендерятся как HTML/CSS/Canvas элементы и сохраняются в видеофайл. Remotion позволяет программно управлять анимацией зума и курсора.12 |
| **Аудио/Видео процессинг** | **FFmpeg (через WASM или child\_process)** | Обработка медиафайлов, склейка потоков, наложение аудио. |

## ---

**4\. Алгоритмическая база: Как это работает**

Для разработки аналога недостаточно выбрать стек технологий. Необходимо реализовать сложные алгоритмы обработки данных.

### **4.1. Алгоритм Автоматического Зума (The Auto-Zoom Heuristic)**

Автозум — это не просто увеличение картинки. Это алгоритм операторской работы (Virtual Camera Director).

**Логика работы:**

1. **Сбор данных (Recording Phase)**:  
   * При каждом клике мыши записывается событие: { timestamp, x, y, windowRect, actionType }.  
   * windowRect получается через Accessibility API (размер кнопки или поля ввода). Если API недоступен, берется стандартная область вокруг курсора (например, 400x300 px).  
2. **Кластеризация событий (Processing Phase)**:  
   * Если пользователь делает серию быстрых кликов в одной области (например, выбирает пункты в меню), алгоритм объединяет их в один "Сюжетный блок". Камера не должна дергаться между соседними кнопками.  
3. **Расчет траектории камеры**:  
   * **Target Viewport**: Вычисляется прямоугольник, который должен быть показан. Он должен включать элемент интерфейса \+ отступы (padding) для контекста.  
   * **Easing**: Переход от полного экрана к зуму (Zoom In) и обратно (Zoom Out) должен использовать функции плавности (например, cubic-bezier(0.25, 1, 0.5, 1)), чтобы движение казалось естественным, а не механическим.  
   * **Lookahead**: Алгоритм "смотрит в будущее". Зум должен начаться *до* того, как произойдет клик (за 500-800 мс), чтобы к моменту действия зритель уже видел крупный план.

### **4.2. Алгоритм Сглаживания Курсора (Virtual Cursor Pathing)**

Сырые данные мыши содержат микро-дрожания и неравномерную скорость.

**Этапы обработки:**

1. **Упрощение пути (Path Simplification)**:  
   * Используется алгоритм **Рамера-Дугласа-Пекера (Ramer-Douglas-Peucker)**. Он удаляет лишние точки на прямых участках, оставляя только ключевые узлы, где меняется направление движения.22  
2. **Генерация сплайна (Spline Interpolation)**:  
   * Оставшиеся точки используются как контрольные для построения кривой. Чаще всего используются **Сплайны Катмулла-Рома (Catmull-Rom Splines)**, так как они гарантированно проходят через все контрольные точки (в отличие от B-сплайнов, которые могут сглаживать путь, не касаясь точек). Это важно, чтобы курсор точно попадал в место клика.23  
3. **Рендеринг**:  
   * Виртуальный курсор (SVG) движется по вычисленной кривой. В моменты простоя (idle) курсор может плавно исчезать.

### **4.3. Неразрушающая архитектура проекта (State-Based Project File)**

Вместо того чтобы рендерить эффекты в пиксели сразу, система хранит состояние.

**Структура project.json (Гипотетическая):**

JSON

{  
  "assets": {  
    "screenVideo": "raw\_capture\_no\_cursor.mp4",  
    "webcamVideo": "webcam\_raw.mp4",  
    "cursorData": "telemetry.json"  
  },  
  "timeline": {  
    "zooms":,  
    "cursorSettings": {  
      "size": 32,  
      "smoothingFactor": 0.8,  
      "style": "macos-monterey"  
    },  
    "background": {  
      "type": "gradient",  
      "colors": \["\#ff00cc", "\#333399"\]  
    }  
  }  
}

При воспроизведении в редакторе движок считывает этот JSON и на лету трансформирует screenVideo с помощью CSS-трансформаций (transform: scale(...) translate(...)) или GPU-шейдеров.

## ---

**5\. Руководство по разработке аналога: Проект "OpenStudio"**

Разработка собственного аналога — амбициозная задача, требующая интеграции системного программирования (Rust/C++) и высокоуровневого UI (React/Web). Ниже представлен детальный план (Blueprint) разработки.

### **Этап 1: Выбор стека технологий**

Для максимальной производительности и кроссплатформенности (Windows \+ macOS) рекомендуется следующий гибридный стек:

1. **Backend (Core Logic & Capture)**: **Rust \+ Tauri v2**.  
   * *Почему Rust?* Безопасная работа с памятью, высокая производительность, отличный доступ к нативным API через FFI.  
   * *Почему Tauri?* Значительно легче Electron, использует системный WebView, имеет мощную систему плагинов для взаимодействия с OS.25  
2. **Frontend (UI & Editor)**: **React \+ TypeScript**.  
3. **Video Rendering**: **Remotion**.  
   * Библиотека для создания видео программным способом с использованием React. Идеально подходит для реализации таймлайна, слоев и композитинга.12  
4. **Database / Metadata**: **JSON / SQLite** (для хранения логов событий).

### **Этап 2: Реализация модуля захвата (The Recorder)**

Это самая сложная часть, требующая написания платформо-зависимого кода на Rust.

#### **2.1. Захват видео (No Cursor)**

Нам нужно получать "чистый" видеопоток.

* **macOS (Rust \+ screencapturekit crate)**:  
  * Использовать SCStreamConfiguration.  
  * Установить свойство showsCursor \= false.  
  * Использовать SCContentFilter для исключения окна самого приложения из записи.  
  * Полученные CMSampleBuffer кодировать в H.264 и писать в MP4 файл.  
* **Windows (Rust \+ windows-capture crate)**:  
  * Использовать GraphicsCaptureItem.  
  * Установить CaptureSession.IsCursorCaptureEnabled \= false.27  
  * Получать кадры Direct3D11Surface, кодировать через Media Foundation или FFmpeg.

#### **2.2. Захват метаданных (Telemetry Logger)**

Параллельно с видео нужно запустить поток сбора данных.

* **Глобальный хук ввода**: Использовать Rust-крейт rdev или device\_query для прослушивания событий мыши и клавиатуры в фоновом режиме.  
* **Получение контекста (UI Automation)**:  
  * Когда происходит Click событие, нужно запросить у ОС границы элемента под курсором.  
  * **macOS**: Использовать Accessibility API (AXUIElementCreateSystemWide, AXUIElementCopyElementAtPosition). Получить атрибуты kAXPositionAttribute и kAXSizeAttribute.16  
  * **Windows**: Использовать UI Automation API (IUIAutomation::ElementFromPoint). Это позволит получить CurrentBoundingRectangle кнопки или окна.29  
  * *Оптимизация*: Эти вызовы могут быть медленными. Выполнять их асинхронно, не блокируя основной поток записи событий.

**Формат выходных данных (events.json):**

JSON

\[  
  { "t": 1024, "type": "move", "x": 500, "y": 600 },  
  { "t": 1038, "type": "move", "x": 505, "y": 602 },  
  { "t": 1500, "type": "click", "btn": "left", "x": 510, "y": 605,   
    "context": { "rect": , "app": "VS Code" }   
  }  
\]

### **Этап 3: Реализация редактора (The Editor)**

После записи пользователь попадает в редактор. Здесь мы используем **Remotion**.

#### **3.1. Визуализация Виртуальной Камеры**

В Remotion видео — это React-компонент. Чтобы реализовать зум, мы оборачиваем видео в контейнер и применяем CSS-трансформации.

TypeScript

// Pseudocode for Zoom Component in Remotion  
const ScreenRecording \= ({ zoomLevel, panX, panY, videoSrc }) \=\> {  
  return (  
    \<div style={{   
      overflow: 'hidden',   
      width: '1920px',   
      height: '1080px',  
      background: userBackground   
    }}\>  
      \<div style={{  
        transform: \`scale(${zoomLevel}) translate(${-panX}px, ${-panY}px)\`,  
        transition: 'transform 0.5s cubic-bezier(0.25, 1, 0.5, 1)' // Плавность\!  
      }}\>  
        \<Video src={videoSrc} /\>  
        \<CursorOverlay /\> {/\* Виртуальный курсор внутри трансформируемого слоя \*/}  
      \</div\>  
    \</div\>  
  );  
};

#### **3.2. Рендеринг Курсора**

Компонент \<CursorOverlay /\> читает events.json.

* На основе текущего кадра (frame) определяет положение курсора.  
* Если включено "Сглаживание", координаты вычисляются не линейной интерполяцией между точками ![][image2] и ![][image3], а через функцию сплайна, учитывающую точки ![][image4] для плавности.  
* Remotion имеет встроенные функции interpolate(), которые идеально подходят для маппинга времени на координаты.24

### **Этап 4: Редактирование зумов (Manual Editing)**

Пользователь должен иметь возможность "переопределить" автозум.

1. **UI Таймлайна**: Отобразить видеодорожку и под ней "дорожку зума".  
2. **Взаимодействие**: Зумы — это прямоугольные блоки на таймлайне.  
   * Пользователь может перетащить границы блока (изменить длительность).  
   * Кликнув на блок, пользователь видит на превью рамку "камеры". Он может перетащить эту рамку в другую часть экрана. Это обновляет координаты targetRect в JSON-состоянии проекта.

### **Этап 5: Экспорт (Rendering Pipeline)**

Когда пользователь нажимает "Экспорт":

1. Tauri запускает процесс рендеринга Remotion.  
2. Remotion использует Headless Chrome (или внутренний механизм) для покадрового рендеринга React-компонентов в изображения.  
3. Эти изображения передаются в FFmpeg.  
4. FFmpeg собирает их в MP4, добавляет аудиодорожку, применяет AAC-кодирование.  
5. *Важно*: Для 4K видео рендеринг в браузере может быть ресурсоемким. Необходимо использовать аппаратное ускорение и оптимизировать работу с памятью (Garbage Collection).31

## ---

**6\. Сравнительная таблица функций и рекомендуемых технологий**

| Функция | Реализация в ScreenStudio/CANVID | Рекомендация для своего аналога (OpenStudio) |
| :---- | :---- | :---- |
| **Захват видео** | ScreenCaptureKit (Mac), WGC (Win) | Крейты Rust: screencapturekit, windows-capture |
| **Скрытие курсора** | Системный флаг при захвате | IsCursorCaptureEnabled \= false |
| **Захват кликов** | Глобальные хуки событий | Крейт Rust: rdev |
| **Контекст клика (Границы кнопок)** | Accessibility API / UIAutomation | Native FFI вызовы к AXUIElement (Mac) и UIAutomation (Win) |
| **Сглаживание курсора** | Сплайны (Catmull-Rom / Bezier) | Библиотеки JS для сплайнов (напр., d3-shape или visx) внутри Remotion |
| **Авто-Зум** | Эвристический анализ логов событий | Собственный алгоритм кластеризации кликов на TypeScript |
| **UI Редактора** | Swift UI (ScreenStudio), Web Tech (CANVID) | React \+ Tailwind CSS \+ Remotion Player |
| **Рендеринг видео** | AVFoundation / FFmpeg | Remotion Renderer \+ FFmpeg static binary |

## ---

**7\. Расширенные возможности и вызовы**

### **7.1. Проблема High-DPI и координат (Retina Displays)**

Одной из главных проблем при разработке является несоответствие "логических" точек и "физических" пикселей.

* На MacBook Pro экран может иметь разрешение 3456×2234 физических пикселей, но логически вести себя как 1728×1117 точек.  
* События мыши приходят в логических точках.  
* Видео захватывается в физических пикселях.  
* **Решение**: Ваш бэкенд (Rust) должен запрашивать у системы текущий Scale Factor (коэффициент масштабирования) монитора и умножать координаты событий на этот коэффициент перед записью в JSON. Иначе курсор на видео будет смещен относительно кнопок.32

### **7.2. "Призрачный" скроллинг**

Когда пользователь скроллит страницу, контент движется, но курсор стоит на месте. Если просто наложить курсор поверх видео, это выглядит нормально. Но если включен "Smart Zoom", камера должна плавно следовать за скроллом, чтобы контент оставался в центре.

* **Реализация**: Отслеживать события scroll wheel. Если обнаружен скролл, плавно смещать translateY камеры в направлении скролла с небольшим отставанием (damping), создавая эффект плавного слежения.

### **7.3. Motion Blur (Размытие в движении)**

Для кинематографичного эффекта курсор должен размываться при быстром движении.

* **CSS-фильтры**: filter: blur(4px) работают, но очень медленны при рендеринге 60fps.  
* **Оптимизация**: Использовать заранее заготовленные спрайты курсора с разной степенью размытия. В зависимости от скорости (расстояния между точками в текущем и предыдущем кадре), подменять изображение курсора на более размытое.

## **8\. Заключение**

Разработка аналога ScreenStudio или CANVID — это задача не столько по работе с видео, сколько по работе с данными. Видеофайл выступает лишь "текстурой", на которую натягивается сложная анимация, управляемая логом событий. Использование современных веб\-технологий (React/Remotion) в связке с производительным системным бэкендом (Rust/Tauri) позволяет создать конкурентоспособный продукт силами небольшой команды, обеспечив при этом кроссплатформенность, недоступную оригинальному ScreenStudio. Ключ к успеху лежит в тонкой настройке алгоритмов сглаживания и "режиссуры" автозума, чтобы поведение виртуальной камеры казалось естественным и интеллектуальным.

#### **Источники**

1. Screen Studio — Professional screen recorder for macOS, дата последнего обращения: февраля 15, 2026, [https://screen.studio/](https://screen.studio/)  
2. CREATE Stunning Screen Recordings on Mac? | Screen Studio Tutorial & Walkthrough, дата последнего обращения: февраля 15, 2026, [https://www.youtube.com/watch?v=\_oh2bKxNbt0](https://www.youtube.com/watch?v=_oh2bKxNbt0)  
3. Adding editing zooms | Screen Studio, дата последнего обращения: февраля 15, 2026, [https://screen.studio/guide/adding-editing-zooms](https://screen.studio/guide/adding-editing-zooms)  
4. Cursor | Screen Studio, дата последнего обращения: февраля 15, 2026, [https://screen.studio/guide/cursor](https://screen.studio/guide/cursor)  
5. Terms of service \- Screen Studio, дата последнего обращения: февраля 15, 2026, [https://screen.studio/legal/terms-of-service](https://screen.studio/legal/terms-of-service)  
6. Data Processing Agreement | Screen Studio, дата последнего обращения: февраля 15, 2026, [https://screen.studio/docs/legal/terms-of-service/data-processing-agreement.pdf](https://screen.studio/docs/legal/terms-of-service/data-processing-agreement.pdf)  
7. Smart Zooms for Screen Recordings | CANVID, дата последнего обращения: февраля 15, 2026, [https://www.canvid.com/features/auto-manual-zoom](https://www.canvid.com/features/auto-manual-zoom)  
8. CANVID | AI-Powered Screen Recorder for Windows & Mac, дата последнего обращения: февраля 15, 2026, [https://www.canvid.com/](https://www.canvid.com/)  
9. Editor: UI & Zooming Basics \- CANVID, дата последнего обращения: февраля 15, 2026, [https://www.canvid.com/support/canvid-editor-ui-zooming-basics](https://www.canvid.com/support/canvid-editor-ui-zooming-basics)  
10. Label layout example \- GitHub Gist, дата последнего обращения: февраля 15, 2026, [https://gist.github.com/ColinEberhardt/27508a7c0832d6e8132a9d1d8aaf231c?permalink\_comment\_id=4180151](https://gist.github.com/ColinEberhardt/27508a7c0832d6e8132a9d1d8aaf231c?permalink_comment_id=4180151)  
11. Looking for a Cofounder to build a product : r/SaaS \- Reddit, дата последнего обращения: февраля 15, 2026, [https://www.reddit.com/r/SaaS/comments/18rvt47/looking\_for\_a\_cofounder\_to\_build\_a\_product/](https://www.reddit.com/r/SaaS/comments/18rvt47/looking_for_a_cofounder_to_build_a_product/)  
12. Starting the Studio | Remotion | Make videos programmatically, дата последнего обращения: февраля 15, 2026, [https://www.remotion.dev/docs/studio/](https://www.remotion.dev/docs/studio/)  
13. WWDC22: Take ScreenCaptureKit to the next level | Apple \- YouTube, дата последнего обращения: февраля 15, 2026, [https://www.youtube.com/watch?v=PcqfIFYnVBI](https://www.youtube.com/watch?v=PcqfIFYnVBI)  
14. screencapturekit 1.5.0 \- Docs.rs, дата последнего обращения: февраля 15, 2026, [https://docs.rs/crate/screencapturekit/latest](https://docs.rs/crate/screencapturekit/latest)  
15. Frequently Asked Questions \- CommandPost \- FCP Cafe, дата последнего обращения: февраля 15, 2026, [https://commandpost.fcp.cafe/faq/](https://commandpost.fcp.cafe/faq/)  
16. How do I get the frontmost window at an arbitrary screen location? \- Stack Overflow, дата последнего обращения: февраля 15, 2026, [https://stackoverflow.com/questions/79092953/how-do-i-get-the-frontmost-window-at-an-arbitrary-screen-location](https://stackoverflow.com/questions/79092953/how-do-i-get-the-frontmost-window-at-an-arbitrary-screen-location)  
17. how to screen capture in wpf? \[duplicate\] \- Stack Overflow, дата последнего обращения: февраля 15, 2026, [https://stackoverflow.com/questions/39119704/how-to-screen-capture-in-wpf](https://stackoverflow.com/questions/39119704/how-to-screen-capture-in-wpf)  
18. Windows Graphics Capture vs DXGI Desktop Duplication \- OBS Studio, дата последнего обращения: февраля 15, 2026, [https://obsproject.com/forum/threads/windows-graphics-capture-vs-dxgi-desktop-duplication.149320/](https://obsproject.com/forum/threads/windows-graphics-capture-vs-dxgi-desktop-duplication.149320/)  
19. Desktop Duplication API vs Windows.Graphics.Capture \- Stack Overflow, дата последнего обращения: февраля 15, 2026, [https://stackoverflow.com/questions/74084077/desktop-duplication-api-vs-windows-graphics-capture](https://stackoverflow.com/questions/74084077/desktop-duplication-api-vs-windows-graphics-capture)  
20. windows\_capture \- Rust \- Docs.rs, дата последнего обращения: февраля 15, 2026, [https://docs.rs/windows-capture](https://docs.rs/windows-capture)  
21. Remotion | Make videos programmatically, дата последнего обращения: февраля 15, 2026, [https://www.remotion.dev/](https://www.remotion.dev/)  
22. illustrator brush smooths itself as bezier. is the algorithm that illustrator uses known? if not are there any similar algorithms that take in input points and returns a smooth handle like that??? : r/howdidtheycodeit \- Reddit, дата последнего обращения: февраля 15, 2026, [https://www.reddit.com/r/howdidtheycodeit/comments/1emkpd0/illustrator\_brush\_smooths\_itself\_as\_bezier\_is\_the/](https://www.reddit.com/r/howdidtheycodeit/comments/1emkpd0/illustrator_brush_smooths_itself_as_bezier_is_the/)  
23. Splines and Bézier Curves and their application in Video Games \- GameLudere, дата последнего обращения: февраля 15, 2026, [https://www.gameludere.com/2021/05/13/splines-and-bezier-curves-and-their-application-in-video-games/](https://www.gameludere.com/2021/05/13/splines-and-bezier-curves-and-their-application-in-video-games/)  
24. Zooming in and out over the duration of the video · remotion-dev · Discussion \#639 \- GitHub, дата последнего обращения: февраля 15, 2026, [https://github.com/orgs/remotion-dev/discussions/639](https://github.com/orgs/remotion-dev/discussions/639)  
25. Building a Video generation app in Tauri \- Reddit, дата последнего обращения: февраля 15, 2026, [https://www.reddit.com/r/tauri/comments/1kr3at8/building\_a\_video\_generation\_app\_in\_tauri/](https://www.reddit.com/r/tauri/comments/1kr3at8/building_a_video_generation_app_in_tauri/)  
26. \[feat\] Expose methods to get cursor\_position · Issue \#9250 · tauri-apps/tauri \- GitHub, дата последнего обращения: февраля 15, 2026, [https://github.com/tauri-apps/tauri/issues/9250](https://github.com/tauri-apps/tauri/issues/9250)  
27. Screen Capture Source for .NET Video SDK \- VisioForge Help, дата последнего обращения: февраля 15, 2026, [https://www.visioforge.com/help/docs/dotnet/videocapture/video-sources/screen/](https://www.visioforge.com/help/docs/dotnet/videocapture/video-sources/screen/)  
28. Newest 'accessibility-api' Questions \- Stack Overflow, дата последнего обращения: февраля 15, 2026, [https://stackoverflow.com/questions/tagged/accessibility-api?tab=Newest](https://stackoverflow.com/questions/tagged/accessibility-api?tab=Newest)  
29. get any UI element's on screen position for any application in windows \- Stack Overflow, дата последнего обращения: февраля 15, 2026, [https://stackoverflow.com/questions/2830040/get-any-ui-elements-on-screen-position-for-any-application-in-windows](https://stackoverflow.com/questions/2830040/get-any-ui-elements-on-screen-position-for-any-application-in-windows)  
30. Test Run: The Microsoft UI Automation Library, дата последнего обращения: февраля 15, 2026, [https://learn.microsoft.com/en-us/archive/msdn-magazine/2008/february/test-run-the-microsoft-ui-automation-library](https://learn.microsoft.com/en-us/archive/msdn-magazine/2008/february/test-run-the-microsoft-ui-automation-library)  
31. Optimizing for speed | Remotion | Make videos programmatically, дата последнего обращения: февраля 15, 2026, [https://www.remotion.dev/docs/lambda/optimizing-speed](https://www.remotion.dev/docs/lambda/optimizing-speed)  
32. I created a desktop magnifier using Vue and Tauri (open source) : r/webdev \- Reddit, дата последнего обращения: февраля 15, 2026, [https://www.reddit.com/r/webdev/comments/138hckv/i\_created\_a\_desktop\_magnifier\_using\_vue\_and\_tauri/](https://www.reddit.com/r/webdev/comments/138hckv/i_created_a_desktop_magnifier_using_vue_and_tauri/)

[image1]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAC0AAAAZCAYAAACl8achAAAClUlEQVR4Xu2WTahNURTHl1CEEClRIokoAx8hJUU9AwYMKDI2MKIoSiYGRkrJhGQgpRShDAxuvaGJASmlEBlIIuQjH//fW2d3913n3OPc1DuT+6t/7561znln7b0+9jEb0i4TC7XFZGlmNNbBAxekCdExjhDwNWsYAwFflC5HRwvMkY5ag8D3SM+lpdHREq+k9dGYc1j6IK2KjhYZkX4UfyvpSPekKcHeJnOlJ9J1aVLwjfFFOhGNGUyTedZdVLwehOnmNfsvqGca8oW0oNfl/JF2RGMBAVJf6Kt0VnokvZbeS7u7t9ZCEJThG+mzlcca/bQs2I5LP6XNwT7GW2lJNIqV0p3smgBZIAFcLX6TvibcNe+ZqdJ9aWPmW2FeomQhZ5v02zz4Ei+l+dEoNkm7susz5oEC9281D6IJ7DLltMa8HPPn9kvns+tEurcUNOnvF3QOu9AxL4n/4bR1Fw5k7Yr1bk6ib9DQJGhSSMCj0TEAs6WH1rvwNCWqynOdeR9VBv1JWh2N1p0SQArZoTyNW8x3oym8g3flC0+7GesZdpq/80h0AA6CyknlgI+GpFn5TTNy5J8qlI7aS4Uf7StskcXmU6dTXK+V3pk3WxX0EFkhyyX4R1WNcNB81D01b0q+TTg5H0snzYNPbJc+Wk23FxyQvkm3zf/vd/NZHEmbxqSpbHbGFnVFfUV4OA9ultUfKmSsLmjgeQ4YFsoiq8Zm6iGmTiU8zOr7nvMNoVTOmc/XCKXAwXLTfOfYiBvSL/P3RwiWCqCk+rLIPO17o6MhnHB8v1SNLjhkXmobpIXmzXfMerOYYPEPrHxqVsKn4LNobMg0abn1/wZmEvH5e8s8oLoRy31sYmOopRnROI4wr5lEQ4YMyl8BZnqqrawv7AAAAABJRU5ErkJggg==>

[image2]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAA8AAAAZCAYAAADuWXTMAAAA00lEQVR4XmNgGNaAFYidgVgaXYIYEAPE/4HYF12CEBAD4isMEM1FaHIEwUkoBmleiCaHF4D8CnJyFQMZmv2AmAeIyxkgmg9A+QSBMBDvg7KDGEjQrMIAUagA5ZsC8TcgfgLEMlAxrIAFiGcBcQaSmDEQf4ViEBsnsAXinwwQZ6JjkO0gV2AFIP/sBGJXNHFJIH7IQCChgJw6lwESRchAEIhPM0A0R6PJgRUbAfF5BuzpV4IBEvIgzTORJfSB+BNUAoRB/rVEkr+IJAfDk5DkR8EwBwCnpTFO7iO2SwAAAABJRU5ErkJggg==>

[image3]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAYCAYAAADzoH0MAAAA7UlEQVR4Xu2SvQ4BURBGRygkhAiNQqOT6CS8gCfQaTQKCr0n8AKiIkqdVieiV9CoRCIalYiEUvjG3P2b3X2DPcnJTfabvXfu7BJFaCpwAc/wajzAOZwauzBjvRBGEx5hQT2PwQn8wrzKPIzgEiZ0AIYkG9R0YJGEK9jTAXk70N3ZlOGNgk+owwf86MANt88nzMgZHA/xAjswZVcGkIZbkg2Kyj18wjaMm3of/BnvRk0Jnkg276vMZkxSwKuGW1+T5Lz6yMEdSUFLZUyVZICcD1T2h6f+Jmmfr6LZkHN61h1Y9+YwzBdskPwHERE+fhXfN525yFNZAAAAAElFTkSuQmCC>

[image4]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAFgAAAAZCAYAAAC1ken9AAADrElEQVR4Xu2YS6hPURSHlzwir0QeIRIiilJEXoViQJKBEENKJhSRiddAFJGJSAYSGZAUUm5RBiZSIlKXRBSiKMljfa2z7z133bPPOf/HzR2cr3659tqPdfZZe+11/iIVFRUVTWGxqpdv7MYMUI1SjVD1TrX3cf/vFgxVvRJztl4OqB6r3iRivsuqM4n2qya29a4fNvWQ6q/qs+qd6oeqn2qc6q5qalvvcvCyrkm77+ictPuOGvJ9p+q1mPP1skS1UfVc7OHXOn1N2vdJfRHGBh5R/RbzdXDKNkh1W+ylPlINSdnKgD9p369LR9/xmfZjSd/S0Jko4+19V83qaK6Z+apfqvfeIDY3a+Dobmcrgshk3C5VD2cL7BXrs8obSpL2fYKzwXTVl0T8XQqcuaOaJubcyo7mmtkiNs9Nb1CWq/4kWupseRC5N1RXJD961qg+Se3pIbBDzPcWsZThoa1FrA/PWchw1UPVArHUwECcrBcuyKsSj1ByIzbyWd5GebaKjSuKGnxvkezNKUPw/bA3JKQ3ONanAwdVJ8SO3ECJb0xZuCDJgVkROkZs/qNiEVkW5nwmNraowpmjWugba6BVsn0PkNfJ7/hy0tk6wUOS1ANsMgMvpNpqhZfDHBxnNpRTgSaJRe0pscuoFngQ5szK6c0mpLa+3pBAOqVP3ktog831kVS0QBGMZY6s/ET0hc3368bAjzDnfWdrNqxVdILPi/XhROWWs2NVL8U6e7VI/TmsVewW5jbOgvm/qWZ4Q4R0zuPfIrb5hhqgaiiKzLdivnAnRCEVkHfJvx4GN/KxgYNPVcO8IaHWDYazUm6DqYkbKTFXiAXIaNceCCmUizpdf3eCioHKgQrCwwSNfGww/qJk16mkBey3kr/Lsk7aj2UM1qM+LroE86AqoIqIzTFb9UKsHs+kp9jXyUfVTOm8CZRNHG9EvRrsvNmQPmKLM/c81U/V3FQ7cxKtrMv4rPo0RCiKwcNR3K8XWyvQX7VHLE+PTLUH0r5fcrZA8J35Fzkb+8A3AieTCzoKD0YBHhZDFNWBUGB7AdH8QGwRn5vTl1BMfNY+UW1KxniWiX1CM38ek8XmYyP48qQq+aDaLvETkfb9nrNBqMtjahX7zYM7q8vhEvAb3Ew2+IYMxqtWqzaL/T6QjuY88D0Wwd0GjktXQTo67hubCL7zI023JFwgp72hSXAjk2bq/YEmj7Tvsfvjv4OT/BAUy3ONwkU1RTpfus2gq32vqKioqPjP/APV3OjDSK3GmQAAAABJRU5ErkJggg==>