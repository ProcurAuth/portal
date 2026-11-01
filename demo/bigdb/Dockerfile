FROM python:3.13-slim
WORKDIR /app
COPY app.py /app/
USER 65534:65534
EXPOSE 8080
CMD ["python", "-u", "/app/app.py"]
