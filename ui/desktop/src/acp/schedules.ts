import type {
  CreateScheduleRequest_unstable,
  InspectRunningJobResponse_unstable,
  KillRunningJobResponse_unstable,
  ScheduledJobDto,
  SessionInfo,
} from '@aaif/goose-sdk';
import { getAcpClient } from './acpConnection';

function acpErrorMessage(error: unknown): string | null {
  if (typeof error !== 'object' || error === null) {
    return null;
  }

  const candidate = 'error' in error && isRecord(error.error) ? error.error : error;
  if (!isRecord(candidate)) {
    return null;
  }
  if (typeof candidate.data === 'string') {
    return candidate.data;
  }
  return typeof candidate.message === 'string' ? candidate.message : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function normalizeAcpError(error: unknown, fallback: string): Error {
  const message = acpErrorMessage(error);
  if (message) {
    return new Error(message);
  }
  if (error instanceof Error) {
    return error;
  }
  return new Error(fallback);
}

export async function acpListSchedules(): Promise<ScheduledJobDto[]> {
  try {
    const client = await getAcpClient();
    const response = await client.goose.schedulesList_unstable({});
    return response.jobs;
  } catch (error) {
    throw normalizeAcpError(error, 'Failed to list schedules');
  }
}

export async function acpCreateSchedule(
  request: CreateScheduleRequest_unstable
): Promise<ScheduledJobDto> {
  try {
    const client = await getAcpClient();
    const response = await client.goose.schedulesCreate_unstable(request);
    return response.job;
  } catch (error) {
    throw normalizeAcpError(error, 'Failed to create schedule');
  }
}

export async function acpDeleteSchedule(scheduleId: string): Promise<void> {
  try {
    const client = await getAcpClient();
    await client.goose.schedulesDelete_unstable({ scheduleId });
  } catch (error) {
    throw normalizeAcpError(error, 'Failed to delete schedule');
  }
}

export async function acpListScheduleSessions(
  scheduleId: string,
  limit: number
): Promise<SessionInfo[]> {
  try {
    const client = await getAcpClient();
    const response = await client.goose.schedulesSessionsList_unstable({ scheduleId, limit });
    return response.sessions;
  } catch (error) {
    throw normalizeAcpError(error, 'Failed to list schedule sessions');
  }
}

export async function acpRunScheduleNow(scheduleId: string): Promise<string> {
  try {
    const client = await getAcpClient();
    const response = await client.goose.schedulesRunNow_unstable({ scheduleId });
    return response.sessionId;
  } catch (error) {
    throw normalizeAcpError(error, 'Failed to run schedule now');
  }
}

export async function acpPauseSchedule(scheduleId: string): Promise<void> {
  try {
    const client = await getAcpClient();
    await client.goose.schedulesPause_unstable({ scheduleId });
  } catch (error) {
    throw normalizeAcpError(error, 'Failed to pause schedule');
  }
}

export async function acpUnpauseSchedule(scheduleId: string): Promise<void> {
  try {
    const client = await getAcpClient();
    await client.goose.schedulesUnpause_unstable({ scheduleId });
  } catch (error) {
    throw normalizeAcpError(error, 'Failed to unpause schedule');
  }
}

export async function acpUpdateSchedule(
  scheduleId: string,
  cron: string
): Promise<ScheduledJobDto> {
  try {
    const client = await getAcpClient();
    const response = await client.goose.schedulesUpdate_unstable({ scheduleId, cron });
    return response.job;
  } catch (error) {
    throw normalizeAcpError(error, 'Failed to update schedule');
  }
}

export async function acpKillRunningJob(jobId: string): Promise<KillRunningJobResponse_unstable> {
  try {
    const client = await getAcpClient();
    return await client.goose.schedulesRunningJobKill_unstable({ jobId });
  } catch (error) {
    throw normalizeAcpError(error, 'Failed to kill running job');
  }
}

export async function acpInspectRunningJob(
  jobId: string
): Promise<InspectRunningJobResponse_unstable> {
  try {
    const client = await getAcpClient();
    return await client.goose.schedulesRunningJobInspect_unstable({ jobId });
  } catch (error) {
    throw normalizeAcpError(error, 'Failed to inspect running job');
  }
}
