import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import DropZone from "./DropZone";

const noop = () => {};

describe("DropZone (presentational)", () => {
  it("shows the drop label when idle", () => {
    render(<DropZone pendingConfirm={null} status={null} isDragOver={false} onAdd={noop} onSkip={noop} />);
    expect(screen.getByText(/drop video files or folders here/i)).toBeInTheDocument();
  });

  it("shows the status line when set and nothing is pending", () => {
    render(<DropZone pendingConfirm={null} status={"Added 1"} isDragOver={false} onAdd={noop} onSkip={noop} />);
    expect(screen.getByText("Added 1")).toBeInTheDocument();
    expect(screen.queryByText(/drop video files/i)).not.toBeInTheDocument();
  });

  it("renders the confirm prompt and wires Add/Skip to the handlers", async () => {
    const onAdd = vi.fn();
    const onSkip = vi.fn();
    render(
      <DropZone
        pendingConfirm={{ file_count: 12, folder_name: "Big", folder_path: "/big" }}
        status={null}
        isDragOver={false}
        onAdd={onAdd}
        onSkip={onSkip}
      />,
    );
    expect(screen.getByText(/Add 12 files from/)).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(screen.getByRole("button", { name: "Skip" }));
    expect(onAdd).toHaveBeenCalledTimes(1);
    expect(onSkip).toHaveBeenCalledTimes(1);
  });

  it("shows the confirm prompt even when a status is also set", () => {
    // Guards the render-gating bug (old DropZone.test.tsx:117): the confirm and a live status
    // can coexist, so the confirm must not be hidden behind the status branch.
    render(
      <DropZone
        pendingConfirm={{ file_count: 12, folder_name: "Big", folder_path: "/big" }}
        status={"Added 1"}
        isDragOver={false}
        onAdd={noop}
        onSkip={noop}
      />,
    );
    expect(screen.getByText(/Add 12 files from/)).toBeInTheDocument();
    expect(screen.getByText("Added 1")).toBeInTheDocument();
  });

  it("applies drag-over styling", () => {
    const { container } = render(
      <DropZone pendingConfirm={null} status={null} isDragOver={true} onAdd={noop} onSkip={noop} />,
    );
    expect(container.querySelector(".drop-zone.drag-over")).not.toBeNull();
  });

  it("renders a pick button instead of the drop label when onPick is given", async () => {
    const onPick = vi.fn();
    render(
      <DropZone pendingConfirm={null} onAdd={vi.fn()} onSkip={vi.fn()} status={null} isDragOver={false} onPick={onPick} />,
    );

    // There is no OS drag-drop event in a browser tab, so advertising one is a lie.
    expect(screen.queryByText(/Drop video files/)).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /Add files or folders/ }));
    expect(onPick).toHaveBeenCalled();
  });

  it("keeps the drop label when onPick is absent", () => {
    render(
      <DropZone pendingConfirm={null} onAdd={vi.fn()} onSkip={vi.fn()} status={null} isDragOver={false} />,
    );
    expect(screen.getByText(/Drop video files/)).toBeInTheDocument();
  });

  it("shows the folder confirm prompt even when onPick is given", () => {
    render(
      <DropZone
        pendingConfirm={{ folder_path: "/m", folder_name: "m", file_count: 9 }}
        onAdd={vi.fn()}
        onSkip={vi.fn()}
        status={null}
        isDragOver={false}
        onPick={vi.fn()}
      />,
    );
    // onPick must not shadow the confirm branch — that would strand the intake pipeline.
    expect(screen.getByText(/Add 9 files/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Add files or folders/ })).not.toBeInTheDocument();
  });
});
