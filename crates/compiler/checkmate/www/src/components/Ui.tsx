import React from "react";
import { AllEvents, Event, UnificationMode } from "../schema";
import { Refine } from "../utils/refine";
import clsx from "clsx";
import { Engine, EventIndex } from "../engine/engine";
import { lastSubEvent } from "../engine/event_util";
import { VariableEl } from "./Variable";

interface UiProps {
  events: AllEvents;
}

export default function Ui({ events }: UiProps): JSX.Element {
  const engine = new Engine(events);

  return (
    <div className="font-mono mt-4">
      <EventList engine={engine} root events={events}></EventList>
    </div>
  );
}

interface EventListProps {
  engine: Engine;
  events: Event[];
  root?: boolean;
}

const MT = "mt-2.5";
const UNFOCUSED = "opacity-40";

function EventList({ engine, events, root }: EventListProps): JSX.Element {
  return (
    <ul className={clsx(MT, root ? "ml-2" : "ml-[1.5em]", "relative")}>
      {events.map((event, i) => (
        <li key={i} className={MT}>
          <OneEvent engine={engine} event={event} />
        </li>
      ))}
    </ul>
  );
}

interface OneEventProps {
  engine: Engine;
  event: Event;
}

function OneEvent({ event, engine }: OneEventProps): JSX.Element {
  switch (event.type) {
    case "Unification":
      return <Unification engine={engine} event={event} />;
    case "VariableUnified":
      return <></>;
    case "VariableSetDescriptor":
      return <></>;
  }
}

const DROPDOWN_CLOSED = "▶";
const DROPDOWN_OPEN = "▼";

const UN_UNKNOWN = "❔";
const UN_SUCCESS = "✅";
const UN_FAILURE = "❌";

interface UnificationProps {
  engine: Engine;
  event: Refine<Event, "Unification">;
}

function Unification({ engine, event }: UnificationProps): JSX.Element {
  const { mode, subevents, success } = event;

  const beforeUnificationIndex = engine.getEventIndex(event);
  const afterUnificationIndex = engine.getEventIndex(lastSubEvent(event));

  const leftVar = (index: EventIndex) => (
    <VariableEl engine={engine} index={index} variable={event.left} />
  );
  const rightVar = (index: EventIndex) => (
    <VariableEl engine={engine} index={index} variable={event.right} />
  );

  const [isOpen, setIsOpen] = React.useState(false);

  const modeIcon = <UnificationModeIcon mode={mode} />;
  const dropdownIcon = isOpen ? DROPDOWN_OPEN : DROPDOWN_CLOSED;

  const resultIcon = success ? UN_SUCCESS : UN_FAILURE;
  const resultHeadline = <Headline icon={resultIcon}></Headline>;
  const topHeadline = (
    <Headline icon={isOpen ? UN_UNKNOWN : resultIcon}></Headline>
  );

  function getHeadline(index: EventIndex) {
    return (
      <button onClick={() => setIsOpen(!isOpen)} className="w-full text-left">
        <span className="text-slate-400 mr-2">{dropdownIcon}</span>
        {topHeadline} {leftVar(index)} {modeIcon} {rightVar(index)}
      </button>
    );
  }

  if (!isOpen) {
    const headLine = getHeadline(afterUnificationIndex);
    return <div className={UNFOCUSED}>{headLine}</div>;
  } else {
    const dropdownTransparent = (
      <span className="text-transparent mr-2">{dropdownIcon}</span>
    );

    const headlineBefore = getHeadline(beforeUnificationIndex);

    const headlineAfter = (
      <div className={MT}>
        {dropdownTransparent}
        {resultHeadline} {leftVar(afterUnificationIndex)} {modeIcon}{" "}
        {rightVar(afterUnificationIndex)}
      </div>
    );

    return (
      <div>
        <div>{headlineBefore}</div>
        <EventList engine={engine} events={subevents} />
        {headlineAfter}
      </div>
    );
  }
}

function Headline({ icon }: { icon: string }): JSX.Element {
  return <span className="">{icon}</span>;
}

function UnificationModeIcon({ mode }: { mode: UnificationMode }): JSX.Element {
  switch (mode.type) {
    case "Eq":
      return <>~</>;
    case "Present":
      return <>+=</>;
    case "LambdaSetSpecialization":
      return <>|~|</>;
  }
}
